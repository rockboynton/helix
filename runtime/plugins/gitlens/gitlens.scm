;; On-demand commit inspection. Native services do history/process work off-thread;
;; this module owns GitHub policy, caching, formatting, and popup behavior.
(require "helix/misc.scm")
(require "helix/components.scm")
(require "gitlens-cache.scm")

(provide gitlens-blame gitlens-clear-cache github-repository format-blame inferred-pulls human-datetime source-reference format-diff)

(define request-id 0)
(define diff-view-id 0)
(define showing-diff? #f)
(define pending-blame-update #f)

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

(define (valid-host? host)
  (and (> (string-length host) 0) (<= (string-length host) 253)
       (every? (lambda (label)
                 (and (> (string-length label) 0) (<= (string-length label) 63)
                      (not (starts-with? label "-")) (not (ends-with? label "-"))
                      (every? (lambda (c)
                                (member c (string->list "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-")))
                              (string->list label))))
               (split-many host "."))))

;; Host-qualified identities prevent public/Enterprise cache and link collisions.
;; A hostname does not identify a forge: configured Git remotes are candidates,
;; and a non-GitHub server simply falls back to local details if its API fails.
(define (repository-authority remote)
  (let loop ([prefixes '("https://" "http://" "ssh://git@" "git://")])
    (if (null? prefixes)
        (and (starts-with? remote "git@")
             (let ([parts (split-many (trim-start-matches remote "git@") ":")])
               (and (= (length parts) 2) (string-append (car parts) "/" (cadr parts)))))
        (if (starts-with? remote (car prefixes))
            (trim-start-matches remote (car prefixes))
            (loop (cdr prefixes))))))

(define (github-repository remote)
  (let ([authority (repository-authority remote)])
    (and authority
         (let* ([identity (trim-end-matches (trim-end-matches authority "/") ".git")]
                [parts (split-many identity "/")])
           (and (= (length parts) 3) (valid-host? (car parts))
                (every? valid-name? (cdr parts))
                (string-append (string-downcase (car parts)) "/" (cadr parts) "/" (list-ref parts 2)))))))

;; Two-component arguments remain supported for existing plugin callers.
(define (repository-host repo)
  (let ([parts (split-many repo "/")])
    (if (= (length parts) 3) (car parts) "github.com")))

(define (repository-path repo)
  (let ([parts (split-many repo "/")])
    (if (= (length parts) 3) (string-append (cadr parts) "/" (list-ref parts 2)) repo)))

(define (repository-url repo)
  (string-append "https://" (repository-host repo) "/" (repository-path repo)))

(define (repository-from-remotes remotes)
  (let loop ([remotes remotes])
    (if (null? remotes) #f
        (or (github-repository (car remotes)) (loop (cdr remotes))))))

(define (valid-pr-number? number)
  (and (string? number) (> (string-length number) 0) (<= (string-length number) 10)
       (not (starts-with? number "0"))
       (every? (lambda (c) (member c (string->list "0123456789"))) (string->list number))))

(define (source-reference data)
  (let ([body (text-field data 'body "")])
    (and (starts-with? body "Source: ")
         (let* ([reference (car (split-many (trim-start-matches body "Source: ") ","))]
                [parts (split-many reference "#")])
           (and (= (length parts) 2)
                (github-repository (string-append "https://github.com/" (car parts)))
                (valid-pr-number? (cadr parts)) parts)))))

(define (repository-for data)
  ;; Commits belong to this fork, including new commits without Source provenance.
  (repository-from-remotes (reverse (hash-ref data 'remotes))))

(define (local-hints data repo)
  (let* ([source (source-reference data)]
         [remotes (hash-try-get data 'remotes)]
         [hint-repo (or (and (list? remotes) (repository-from-remotes remotes)) repo)])
    (if source
        (list (hash 'number (cadr source) 'title "Original upstream PR" 'inferred #t
                    'html_url (string-append "https://" (repository-host hint-repo) "/" (car source) "/pull/" (cadr source))))
        (inferred-pulls (text-field data 'title "")
                        hint-repo))))

(define (human-datetime datetime)
  (with-handler (lambda (_) datetime)
    (let* ([months '("Jan" "Feb" "Mar" "Apr" "May" "Jun" "Jul" "Aug" "Sep" "Oct" "Nov" "Dec")]
           [month (exact (string->number (substring datetime 5 7)))]
           [day (exact (string->number (substring datetime 8 10)))]
           [offset (substring datetime 19 (string-length datetime))])
      (string-append (list-ref months (- month 1)) " " (number->string day) ", "
                     (substring datetime 0 4) " at " (substring datetime 11 16)
                     " (UTC" (if (or (equal? offset "Z") (equal? offset "+00:00")) "" offset) ")"))))

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
                    'html_url (string-append (repository-url repo) "/pull/" number)))
        '())))

(define (canonical-pr-url repo pr)
  (let ([url (hash-try-get pr 'html_url)] [number (hash-try-get pr 'number)]
        [prefix (string-append "https://" (repository-host repo) "/")])
    (and (string? url) (<= (string-length url) 4096) (starts-with? url prefix)
         (let ([parts (split-many (trim-start-matches url prefix) "/")])
           (and (= (length parts) 4)
                (valid-name? (car parts)) (valid-name? (cadr parts))
                (equal? (list-ref parts 2) "pull")
                (equal? (list-ref parts 3) number) (valid-pr-number? number)
                url)))))

(define (safe-pulls lookup-repo pulls)
  (if (not (list? pulls)) '()
      ;; PR ownership need not match the repository used to look up a commit.
      ;; Retain the canonical GitHub URL after strict structural validation.
      (filter (lambda (pr)
                     (and (hash? pr) (valid-pr-number? (hash-try-get pr 'number))
                          (string? (hash-try-get pr 'title))
                          (<= (string-length (hash-ref pr 'title)) 1000)
                          (canonical-pr-url lookup-repo pr))) pulls)))

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
    "**Authored:** " (human-datetime (text-field data 'datetime "Unknown")) "\n"
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
        (list (list "Commit" (string-append (repository-url
                                           (or (repository-from-remotes (reverse (hash-ref data 'remotes))) repo)) "/commit/"
                                           (hash-ref data 'commit))))
        '())
    (map (lambda (pr)
           (list (string-append "PR #" (text-field pr 'number "?"))
                 (text-field pr 'html_url ""))) pulls)))

(define (show-blame! id snapshot repo pulls note)
  (when (and (= id request-id) (line-blame-current? snapshot))
    (set-status! "Line blame ready")
    (let ([data (hash-ref snapshot 'data)])
      (let ([popup (markdown-popup (format-blame data pulls note)
                                   (popup-links data repo pulls))])
        (markdown-popup-keybinding! popup #\d (lambda () (show-diff! snapshot)))
        (markdown-popup-keybinding! popup #\y
          (lambda () (copy-text! (hash-ref data 'commit) "Commit hash")))
        (if showing-diff?
            (set! pending-blame-update popup)
            (update-markdown-popup! popup))))))

(define (format-diff patch)
  (string-append "# Commit diff\n\n" (markdown-code-block "diff" patch)))

(define (diff-popup content data)
  (let ([popup (markdown-popup content '())])
    (markdown-popup-keybinding! popup #\y
      (lambda () (copy-text! (hash-ref data 'commit) "Commit hash")))
    (markdown-popup-keybinding! popup #\d
      (lambda ()
        ;; A pending diff reply must not replace the restored original view.
        (set! diff-view-id (+ diff-view-id 1))
        (set! showing-diff? #f)
        (restore-markdown-popup!)
        (when pending-blame-update
          (update-markdown-popup! pending-blame-update)
          (set! pending-blame-update #f))
        (set-status! "Line blame ready")))
    popup))

(define (show-diff! snapshot)
  (when (line-blame-current? snapshot)
    ;; PR enrichment continues, but its UI update is deferred until returning.
    (set! diff-view-id (+ diff-view-id 1))
    (set! showing-diff? #t)
    (let* ([id request-id]
           [view-id diff-view-id]
           [data (hash-ref snapshot 'data)]
           [commit (hash-ref data 'commit)])
      (switch-markdown-popup! (diff-popup "# Commit diff\n\nLoading…" data))
      (await-callback
        (run-command-async "git"
          (list "-C" (hash-ref snapshot 'directory) "--no-pager" "show"
                "--no-ext-diff" "--no-textconv" "--format=fuller" "--patch"
                "--diff-merges=first-parent" commit "--") 5000)
        (lambda (result)
          (when (and (= id request-id) (= view-id diff-view-id) showing-diff?
                     (line-blame-current? snapshot))
            (update-markdown-popup!
              (diff-popup
                (if (hash-ref result 'ok) (format-diff (hash-ref result 'stdout))
                    "# Commit diff\n\nDiff unavailable: Git failed, timed out, or the patch exceeded the 1 MiB limit.")
                data))))))))

(define (unpublished-commit? result)
  (equal? (hash-try-get result 'unpublished) #t))

(define (refresh-pulls! id snapshot repo fallback)
  (let ([commit (hash-ref (hash-ref snapshot 'data) 'commit)]
        [cache-generation (pr-cache-generation)])
    (await-callback
      (github-commit-pulls repo commit)
      (lambda (result)
        (if (hash-ref result 'ok)
            (with-handler
              (lambda (_) (show-blame! id snapshot repo fallback "PR: response unavailable; showing local hints/cached details."))
              (let* ([raw (hash-ref result 'pulls)]
                     [pulls (if (list? raw) (safe-pulls repo raw) (error "Expected PR array"))])
                (when (= cache-generation (pr-cache-generation))
                  (remember-prs! repo commit pulls))
                (show-blame! id snapshot repo (if (null? pulls) fallback pulls)
                             (if (null? pulls) "PR: no associated PR found. Message hints remain unverified." "PR: verified by GitHub."))))
            (if (unpublished-commit? result)
                (show-blame! id snapshot #f fallback
                  "PR: commit not published in this GitHub repository; source/message hints remain available.")
                (show-blame! id snapshot repo fallback
                  (string-append "PR: " (text-field result 'error "lookup unavailable") "; local details remain available."))))))))

(define (enrich-blame! id snapshot repo)
  (let* ([data (hash-ref snapshot 'data)]
         [commit (hash-ref data 'commit)]
         [hints (local-hints data repo)])
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
  (github-clear-clients!)
  (set-status! "GitLens saved PR cache cleared"))

(define (gitlens-blame)
  (set! request-id (+ request-id 1))
  (set! diff-view-id (+ diff-view-id 1))
  (set! showing-diff? #f)
  (set! pending-blame-update #f)
  (let ([id request-id]
        [future (line-blame-at-cursor)])
    (set-status! "Looking up line blame…")
    (pop-last-component-by-name! "steel-markdown-links")
    (push-component! (markdown-popup "# Line blame\n\nLoading commit details…" '()))
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
                   (show-blame! id snapshot #f '() "No supported repository remote; showing local commit details.")))]))))))
