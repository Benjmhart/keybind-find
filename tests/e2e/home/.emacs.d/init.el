(global-set-key (kbd "C-c k") (quote kill-buffer))
;; In C buffers only: what the running Emacs lists and the batch read of this file does not.
(add-hook (quote c-mode-hook) (lambda () (local-set-key (kbd "C-c j") (quote kill-buffer))))
