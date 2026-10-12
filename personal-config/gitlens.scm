;; On-demand commit inspection. Native services do history/process work off-thread;
;; this module owns GitHub policy, caching, formatting, and the command binding.
(require "helix/misc.scm")
(require "helix/components.scm")
(require "helix/keymaps.scm")
(require-builtin steel/time)

(provide gitlens-blame configure-gitlens! github-repository format-blame)

(define request-id 0)
(define pr-cache (hash))
(define cache-ttl-ms 300000)
(define cache-limit 128)

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

;; Commit messages are displayed as text, not interpreted as Markdown commands/links.
(define (literal-text text)
  (apply string-append
         (map (lambda (c)
                (if (member c '(#\* #\_ #\[ #\] #\` #\< #\> #\# #\\))
                    (string-append "\\" (string c))
                    (string c)))
              (string->list text))))

(define (format-blame data line pulls note)
  (string-append
    "# Line blame\n\n"
    "**Author:** " (literal-text (text-field data 'author "Unknown"))
    " <" (literal-text (text-field data 'email "unknown email")) ">\n\n"
    "**Authored:** " (text-field data 'datetime "Unknown") "\n\n"
    "**Commit:** " (text-field data 'commit "Unknown") "\n\n"
    "**Line:** " (number->string (exact (+ line 1))) "\n\n"
    "## " (literal-text (text-field data 'title "Untitled commit")) "\n\n"
    (literal-text (text-field data 'body "")) "\n\n"
    (apply string-append
      (map (lambda (pr)
             (string-append "**PR #" (text-field pr 'number "?") ":** "
                            (literal-text (text-field pr 'title "Untitled"))
                            " (" (if (string? (hash-try-get pr 'merged_at)) "merged"
                                      (text-field pr 'state "unknown")) ")\n\n")) pulls))
    note))

(define (popup-links data repo pulls)
  (append
    (map (lambda (pr)
           (list (string-append "PR #" (text-field pr 'number "?"))
                 (text-field pr 'html_url ""))) pulls)
    (if repo
        (list (list "Commit" (string-append "https://github.com/" repo "/commit/"
                                           (hash-ref data 'commit))))
        '())))

(define (show-blame! id snapshot repo pulls note)
  (when (and (= id request-id) (line-blame-current? snapshot))
    (set-status! "Line blame ready")
    (let ([data (hash-ref snapshot 'data)])
      (update-markdown-popup!
        (markdown-popup (format-blame data (hash-ref snapshot 'line) pulls note)
                        (popup-links data repo pulls))))))

(define (cached-pulls key)
  (let ([entry (hash-try-get pr-cache key)])
    (and entry (< (- (current-milliseconds) (car entry)) cache-ttl-ms) (cadr entry))))

(define (remember-pulls! key pulls)
  (when (>= (hash-length pr-cache) cache-limit) (set! pr-cache (hash)))
  (set! pr-cache (hash-insert pr-cache key (list (current-milliseconds) pulls))))

(define (enrich-blame! id snapshot repo)
  (let* ([commit (hash-ref (hash-ref snapshot 'data) 'commit)]
         [key (string-append repo "/" commit)]
         [cached (cached-pulls key)])
    (if cached
        (show-blame! id snapshot repo cached "")
        (begin
          ;; Show local data immediately; GitHub enrichment never gates the popup.
          (show-blame! id snapshot repo '() "Looking up associated GitHub PRs…")
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
                    (lambda (_) (show-blame! id snapshot repo '() "Could not decode GitHub's response."))
                    (let ([pulls (string->jsexpr (hash-ref result 'stdout))])
                      (remember-pulls! key pulls)
                      (show-blame! id snapshot repo pulls
                                   (if (null? pulls) "No associated PR found." ""))))
                  (show-blame! id snapshot repo '()
                    "PR lookup unavailable (offline, gh missing/unauthed, or API error). Local blame still works."))))))))

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
