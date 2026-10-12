;; Lazy, bounded, private PR metadata cache. Filesystem work uses async services.
(require "helix/misc.scm")
(require-builtin steel/time)

(provide with-pr-cache! cached-pr-entry remember-prs! clear-pr-cache! pr-cache-generation)

(define entries (hash))
(define loaded? #f)
(define loading? #f)
(define waiters '())
(define writing? #f)
(define dirty? #f)
(define generation 0)
(define cache-name "gitlens-pr-v1")
(define fresh-ms 300000)
(define retain-ms 604800000)
(define entry-limit 128)

(define (pr-cache-generation) generation)

(define (entry-valid? entry)
  (and (list? entry) (= (length entry) 2) (number? (car entry))
       (list? (cadr entry)) (<= (length (cadr entry)) 20)
       (let ([age (- (current-milliseconds) (car entry))])
         (and (>= age 0) (< age retain-ms)))))

(define (cache-key repo commit)
  ;; JSON object keys decode as symbols in Steel; use the same representation in memory.
  (string->symbol (string-append repo "/" commit)))

(define (cached-pr-entry repo commit)
  (let ([entry (hash-try-get entries (cache-key repo commit))])
    (and (entry-valid? entry)
         (list (cadr entry) (< (- (current-milliseconds) (car entry)) fresh-ms)))))

(define (prune-oldest!)
  (when (>= (hash-length entries) entry-limit)
    (let ([oldest #f])
      (for-each
        (lambda (key)
          (when (or (not oldest)
                    (< (car (hash-ref entries key)) (car (hash-ref entries oldest))))
            (set! oldest key)))
        (hash-keys->list entries))
      (when oldest (set! entries (hash-remove entries oldest))))))

(define (persist!)
  (set! dirty? #t)
  (unless writing?
    (set! writing? #t)
    (set! dirty? #f)
    (await-callback
      (store-plugin-cache! cache-name (hash 'version "1" 'entries entries))
      (lambda (_)
        (set! writing? #f)
        ;; Coalesce concurrent results; never let an older snapshot win this process's write race.
        (when dirty? (persist!))))))

(define (remember-prs! repo commit pulls)
  (let ([key (cache-key repo commit)])
    (unless (hash-contains? entries key) (prune-oldest!))
    (set! entries (hash-insert entries key (list (current-milliseconds) pulls)))
    (persist!)))

(define (with-pr-cache! continuation)
  (if loaded?
      (continuation)
      (begin
        (set! waiters (cons continuation waiters))
        (unless loading?
          (set! loading? #t)
          (let ([requested-generation generation])
            (await-callback
              (load-plugin-cache cache-name)
              (lambda (saved)
                (when (= requested-generation generation)
                  (when (and (hash? saved) (equal? (hash-try-get saved 'version) "1")
                             (hash? (hash-try-get saved 'entries)))
                    (for-each
                      (lambda (key)
                        (let ([entry (hash-ref (hash-ref saved 'entries) key)])
                          (when (and (< (hash-length entries) entry-limit) (entry-valid? entry))
                            (set! entries (hash-insert entries key entry)))))
                      (hash-keys->list (hash-ref saved 'entries))))
                  (set! loaded? #t)
                  (set! loading? #f)
                  (let ([pending waiters])
                    (set! waiters '())
                    (for-each (lambda (resume) (resume)) (reverse pending)))))))))))

(define (clear-pr-cache!)
  (set! generation (+ generation 1))
  (set! entries (hash))
  (set! loaded? #t)
  (set! loading? #f)
  (set! waiters '())
  (persist!))
