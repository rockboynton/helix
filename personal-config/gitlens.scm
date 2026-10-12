;; On-demand commit inspection. Native services do history/process work off-thread;
;; this module owns GitHub policy, caching, formatting, and the command binding.
(require "helix/misc.scm")
(require "helix/components.scm")
(require "helix/keymaps.scm")
(require "gitlens-cache.scm")

(provide gitlens-blame gitlens-clear-cache configure-gitlens! github-repository format-blame inferred-pulls)

(define request-id 0)

(define (every? predicate values)
  (or (null? values)
      (and (predicate (car values)) (every? predicate (cdr values)))))

(define (text-field data key fallback)
  (let ([value (hash-try-get data key)])
    (if (string? value) value fallback)))

(define (valid-name? name)
  (and (not (equal? name ""))
       (not (member name '("." "..")))
       (every? (lambda (c)
                 (member c (string->list "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_.")))
               (string->list name))))

(define (github-repository remote)
  (let loop ([prefixes '("https://github.com/" "http://github.com/"
                         "ssh://git@github.com/" "git@github.com:"
                         "git://github.com/")])
    (if (null? prefixes)
        #f
        (if (starts-with? remote (car prefixes))
            (let* ([path (trim-end-matches
                          (trim-end-matches (trim-start-matches remote (car prefixes)) "/")
                          ".git")]
                   [parts (split-many path "/")])
              (and (= (length parts) 2) (every? valid-name? parts) path))
            (loop (cdr prefixes))))))

(define (repository-for data)
  (let loop ([remotes (hash-ref data 'remotes)])
    (if (null? remotes) #f
        (or (github-repository (car remotes)) (loop (cdr remotes))))))

(define (valid-pr-number? number)
  (and (string? number) (> (string-length number) 0) (<= (string-length number) 10)
       (not (starts-with? number "0"))
       (every? (lambda (c) (member c (string->list "0123456789"))) (string->list number))))

(define (message-pr-number title)
  (cond
    [(starts-with? title "Merge pull request #")
     (let ([parts (split-many (trim-start-matches title "Merge pull request #") " from ")])
       (and (= (length parts) 2) (valid-pr-number? (car parts)) (car parts)))]
    [(ends-with? title ")")
     (let ([parts (split-many title " (#")])
       (and (= (length parts) 2)
            (let ([number (substring (cadr parts) 0 (- (string-length (cadr parts)) 1))])
              (and (valid-pr-number? number) number))))]
    [else #f]))

(define (inferred-pulls title repo)
  (let ([number (and repo (message-pr-number title))])
    (if number
        (list (hash 'number number 'title "Inferred from commit message" 'inferred #t
                    'state "unverified"
                    'html_url (string-append "https://github.com/" repo "/pull/" number)))
        '())))

(define (safe-pulls repo pulls)
  (if (not (list? pulls)) '()
      (map (lambda (pr)
             ;; Build links from validated numbers/repository, not cache/API URLs.
             (hash-insert pr 'html_url (string-append "https://github.com/" repo "/pull/"
                                                    (hash-ref pr 'number))))
           (filter (lambda (pr)
                     (and (hash? pr) (valid-pr-number? (hash-try-get pr 'number))
                          (string? (hash-try-get pr 'title))
                          (<= (string-length (hash-ref pr 'title)) 1000))) pulls))))

;; Commit messages are displayed as text, not interpreted as Markdown commands/links.
(define (literal-text text)
  (apply string-append
         (map (lambda (c)
                (if (member c '(#\* #\_ #\[ #\] #\` #\< #\> #\# #\\))
                    (string-append "\\" (string c))
                    (string c)))
              (string->list text))))

(define (format-blame data pulls note)
  (string-append
    "# Line blame\n\n"
    "**Author:** " (literal-text (text-field data 'author "Unknown"))
    " <" (literal-text (text-field data 'email "unknown email")) ">\n"
    "**Authored:** " (text-field data 'datetime "Unknown") "\n"
    "**Commit:** " (text-field data 'commit "Unknown") "\n"
    "## " (literal-text (text-field data 'title "Untitled commit")) "\n\n"
    (let ([body (trim-end-matches (text-field data 'body "") "\n")])
      (if (equal? body "") "" (string-append (literal-text body) "\n")))
    (apply string-append
      (map (lambda (pr)
             (string-append "**PR #" (text-field pr 'number "?") ":** "
                            (literal-text (text-field pr 'title "Untitled"))
                            " (" (if (hash-try-get pr 'inferred) "unverified message hint"
                                     (if (string? (hash-try-get pr 'merged_at)) "merged"
                                      (text-field pr 'state "unknown"))) ")\n")) pulls))
    note))

(define (popup-links data repo pulls)
  (append
    (if repo
        (list (list "Commit" (string-append "https://github.com/" repo "/commit/"
                                           (hash-ref data 'commit))))
        '())
    (map (lambda (pr)
           (list (string-append "PR #" (text-field pr 'number "?"))
                 (text-field pr 'html_url ""))) pulls)))

(define (show-blame! id snapshot repo pulls note)
  (when (and (= id request-id) (line-blame-current? snapshot))
    (set-status! "Line blame ready")
    (let ([data (hash-ref snapshot 'data)])
      (update-markdown-popup!
        (markdown-popup (format-blame data pulls note)
                        (popup-links data repo pulls))))))

(define (refresh-pulls! id snapshot repo fallback)
  (let ([commit (hash-ref (hash-ref snapshot 'data) 'commit)]
        [cache-generation (pr-cache-generation)])
    (await-callback
      (run-command-async
        "gh" (list "api" "--hostname" "github.com" "-H"
                   "Accept: application/vnd.github+json"
                   "-H" "X-GitHub-Api-Version: 2022-11-28"
                   (string-append "repos/" repo "/commits/" commit "/pulls?per_page=20")
                   "--jq" "[.[] | {number:(.number|tostring), title, html_url, state, merged_at}]")
        5000)
      (lambda (result)
        (if (hash-ref result 'ok)
            (with-handler
              (lambda (_) (show-blame! id snapshot repo fallback "PR: response unavailable; showing local hints/cached details."))
              (let* ([raw (string->jsexpr (hash-ref result 'stdout))]
                     [pulls (if (list? raw) (safe-pulls repo raw) (error "Expected PR array"))])
                (when (= cache-generation (pr-cache-generation))
                  (remember-prs! repo commit pulls))
                (show-blame! id snapshot repo (if (null? pulls) fallback pulls)
                             (if (null? pulls) "PR: no associated PR found. Message hints remain unverified." "PR: verified by GitHub."))))
            (show-blame! id snapshot repo fallback
              "PR: lookup unavailable; local hints/cached details remain available."))))))

(define (enrich-blame! id snapshot repo)
  (let* ([data (hash-ref snapshot 'data)]
         [commit (hash-ref data 'commit)]
         [hints (inferred-pulls (text-field data 'title "") repo)])
    ;; Immediate local content; neither disk cache loading nor GitHub gates it.
    (show-blame! id snapshot repo hints "PR: checking saved cache…")
    (with-pr-cache!
      (lambda ()
        (when (and (= id request-id) (line-blame-current? snapshot))
          (let* ([cached (cached-pr-entry repo commit)]
                 [pulls (if cached (safe-pulls repo (car cached)) '())]
                 [available (if (null? pulls) hints pulls)])
            (if (and cached (cadr cached))
                (show-blame! id snapshot repo available
                             (if (null? pulls) "PR: no associated PR in saved lookup. Message hints are unverified." "PR: saved verified details."))
                (begin
                  (show-blame! id snapshot repo available
                               (if cached "PR: cached details; refreshing…" "PR: looking up…"))
                  (refresh-pulls! id snapshot repo available)))))))))

(define (gitlens-clear-cache)
  (set! request-id (+ request-id 1))
  (clear-pr-cache!)
  (set-status! "GitLens saved PR cache cleared"))

(define (gitlens-blame)
  (set! request-id (+ request-id 1))
  (let ([id request-id]
        [future (line-blame-at-cursor)])
    (set-status! "Looking up line blame…")
    (pop-last-component-by-name! "steel-markdown-links")
    (push-component! (markdown-popup "# Line blame\n\nLoading commit details…\n\nEsc: cancel" '()))
    (await-callback
      future
      (lambda (snapshot)
        (when (and (= id request-id) (line-blame-current? snapshot))
          (cond
            [(hash-try-get snapshot 'error)
             (update-markdown-popup!
               (markdown-popup (string-append "# Line blame\n\n"
                                              (literal-text (hash-ref snapshot 'error))) '()))]
            [(not (string? (hash-ref (hash-ref snapshot 'data) 'commit)))
             (update-markdown-popup!
               (markdown-popup "# Line blame\n\nThis line has no committed history." '()))]
            [else
             (let ([repo (repository-for (hash-ref snapshot 'data))])
               (if repo (enrich-blame! id snapshot repo)
                   (show-blame! id snapshot #f '() "No GitHub remote; showing local commit details.")))]))))))

(define (configure-gitlens!)
  ;; Uppercase B is Shift-B in normal mode. Space B retains native statusline blame.
  (add-global-keybinding (hash "normal" (hash "B" ":gitlens-blame"))))
