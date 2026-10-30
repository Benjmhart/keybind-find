;;; synth.el --- keybind-find fixture init -*- lexical-binding: t -*-
(global-set-key (kbd "C-c k") #'kill-whole-line)
(define-key emacs-lisp-mode-map (kbd "C-c e") #'eval-buffer)
;; A remap: `M-u` and the minor mode's `C-c m` run capitalize-word.
(global-set-key [remap upcase-word] #'capitalize-word)
(defvar kbf-minor-mode-map (let ((m (make-sparse-keymap))) (define-key m (kbd "C-c m") #'upcase-word) (define-key m (kbd "C-c k") #'kill-line) m))
(define-minor-mode kbf-minor-mode "Fixture." :keymap kbf-minor-mode-map)
(add-hook 'emacs-lisp-mode-hook #'kbf-minor-mode)
(with-current-buffer (get-buffer-create "fixture.el") (emacs-lisp-mode))
(add-hook 'server-after-make-frame-hook (lambda () (switch-to-buffer "fixture.el")))
(switch-to-buffer "fixture.el")
