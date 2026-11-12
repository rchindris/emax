;;; core.scm --- helpers shared by the built-in scripts.
;;;
;;; Loaded first, before the other built-ins and any user script. The host
;;; functions (on, run-after, regex-match?, terminal-*, agent-*) are
;;; documented in src/engine/script/mod.rs.

;; The first line of `lines` that `re` matches, or #f.
(define (find-line re lines)
  (cond [(null? lines) #f]
        [(regex-match? re (car lines)) (car lines)]
        [else (find-line re (cdr lines))]))

;; Whether `re` matches any line of `lines`.
(define (any-line? re lines)
  (if (find-line re lines) #t #f))

;; Pending debounced calls: key -> timer id.
(define *debounced* (hash))

;; Calls (f) once `ms` milliseconds after the last debounce with this key:
;; each call for the same key replaces the pending one.
(define (debounce key ms f)
  (when (hash-contains? *debounced* key)
    (cancel-timer (hash-ref *debounced* key)))
  (set! *debounced*
        (hash-insert *debounced* key
                     (run-after ms (lambda ()
                                     (set! *debounced* (hash-remove *debounced* key))
                                     (f))))))
