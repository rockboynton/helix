;; Public entry point. Requiring modules defines functions without applying settings.
(require (prefix-in editing. "editing.scm"))
(require (prefix-in appearance. "appearance.scm"))
(require (prefix-in keys. "keybindings.scm"))

(provide configure-personal!)

(define (configure-personal!)
  (editing.configure-editing!)
  (appearance.configure-appearance!)
  (keys.configure-keybindings!))
