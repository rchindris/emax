;;; default.scm --- the built-in configuration of ymacs.
;;;
;;; Compiled into the binary and evaluated before your own files, so these
;;; only need what they change: init.scm (written by you) and settings.scm
;;; (written by the settings editor, s-,). `ymacs --print-default-config`
;;; prints this file.
;;;
;;; The forms:
;;;
;;;   (setq option value ...)            set options (see the list below)
;;;   (define-keys 'keymap "keys" 'command ...)
;;;                                      bind keys; #f instead of a command unbinds
;;;   (define-key 'keymap "keys" 'command), (global-set-key "keys" 'command),
;;;   (global-unset-key "keys")
;;;   (define-major-mode 'name-mode #:lighter "Name" #:parent 'other-mode ...)
;;;   (define-minor-mode 'name-mode #:lighter "Name")
;;;   (define-theme 'name #:inherits 'other-theme #:color "#rrggbb" ...)
;;;
;;; A keymap is global, picker, search, settings or terminal, or the name of
;;; a major or minor mode. Defining a mode or theme that exists changes only
;;; what you give.

;; Editing. Major modes can override fill-column and tab-width.
(setq fill-column        70
      tab-width          4
      default-major-mode 'fundamental-mode
      initial-major-mode 'text-mode)

;; Fonts. #f for font-ui-size: panels follow font-size. line-height is a multiple of the size.
(setq font-family    "monospace"
      font-ui-family "system-ui"
      font-size      14
      font-ui-size   #f
      line-height    1.5)

;; The theme: a name, or '((light . name) (dark . name)) to follow the system appearance.
(setq theme 'ymacs-dark)

;; Terminals. #f for terminal-shell: $SHELL, else /bin/sh; for terminal-socket:
;; the default location. terminal-daemon-idle-exit is in seconds, 0: never.
(setq terminal-shell            #f
      terminal-shell-args       '()
      terminal-term             "xterm-256color"
      terminal-scrollback-lines 10000
      terminal-backend          'daemon
      terminal-replay-bytes     2097152
      terminal-socket           #f
      terminal-daemon-idle-exit 60
      terminal-escape-keys      '("C-x" "M-x" "C-c"))

;; Sessions. #f for session-state-dir: the platform default.
(setq restore-session   #t
      session-state-dir #f)

;;; Themes

(define-theme 'ymacs-dark
  #:background                   "#1e1f22"
  #:foreground                   "#d4d7dd"
  #:emphasis                     "#ffffff"
  #:dim                          "#5c6370"
  #:accent                       "#c678dd"
  #:current-line                 "#26282e"
  #:cursor                       "#61afef"
  #:secondary-cursor             "#e5c07b"
  #:region                       "#3a4458"
  #:search-match                 "#3e4a3a"
  #:search-current               "#d19a66"
  #:match-text                   "#61afef"
  #:modeline                     "#2b2d33"
  #:modeline-preview             "#33304a"
  #:panel                        "#25272c"
  #:panel-border                 "#3b3f48"
  #:row-selected                 "#2f3646"
  #:row-hover                    "#2a2d35"
  #:badge                        "#353a45"
  #:overlay                      "#0b0c0f80"
  #:term-black                   "#3f4451"
  #:term-red                     "#e06c75"
  #:term-green                   "#98c379"
  #:term-yellow                  "#e5c07b"
  #:term-blue                    "#61afef"
  #:term-magenta                 "#c678dd"
  #:term-cyan                    "#56b6c2"
  #:term-white                   "#d4d7dd"
  #:term-bright-black            "#5c6370"
  #:term-bright-red              "#ff7b86"
  #:term-bright-green            "#b1e18b"
  #:term-bright-yellow           "#efb074"
  #:term-bright-blue             "#67cdff"
  #:term-bright-magenta          "#e48bff"
  #:term-bright-cyan             "#63d4e0"
  #:term-bright-white            "#ffffff"
  #:syntax.keyword               "#c678dd"
  #:syntax.string                "#98c379"
  #:syntax.comment               "#7f848e"
  #:syntax.comment.documentation "#8f97a3"
  #:syntax.constant              "#d19a66"
  #:syntax.number                "#d19a66"
  #:syntax.function              "#61afef"
  #:syntax.function.macro        "#56b6c2"
  #:syntax.type                  "#e5c07b"
  #:syntax.constructor           "#e5c07b"
  #:syntax.attribute             "#d19a66"
  #:syntax.property              "#e06c75"
  #:syntax.variable.builtin      "#e06c75"
  #:syntax.variable.parameter    "#d4d7dd"
  #:syntax.label                 "#e06c75"
  #:syntax.escape                "#56b6c2"
  #:syntax.operator              "#56b6c2"
  #:syntax.string.special.key    "#e06c75"
  #:syntax.punctuation           "#abb2bf"
  #:syntax.boolean               "#d19a66"
  #:syntax.punctuation.special   "#7f848e"
  #:syntax.text.title            "#61afef"
  #:syntax.text.literal          "#98c379"
  #:syntax.text.uri              "#e06c75"
  #:syntax.text.reference        "#e5c07b"
  #:syntax.text.emphasis         "#e06c75"
  #:syntax.text.strong           "#d19a66")

(define-theme 'ymacs-light
  #:background                   "#fafafa"
  #:foreground                   "#383a42"
  #:emphasis                     "#000000"
  #:dim                          "#9da0a8"
  #:accent                       "#a626a4"
  #:current-line                 "#f0f0f1"
  #:cursor                       "#4078f2"
  #:secondary-cursor             "#c18401"
  #:region                       "#d7e2fb"
  #:search-match                 "#dcefd6"
  #:search-current               "#e08a3c"
  #:match-text                   "#4078f2"
  #:modeline                     "#e9e9eb"
  #:modeline-preview             "#e6e0f3"
  #:panel                        "#f2f2f3"
  #:panel-border                 "#d4d5d9"
  #:row-selected                 "#dfe6f5"
  #:row-hover                    "#e8e9ec"
  #:badge                        "#e2e3e7"
  #:overlay                      "#00000033"
  #:term-black                   "#383a42"
  #:term-red                     "#e45649"
  #:term-green                   "#50a14f"
  #:term-yellow                  "#c18401"
  #:term-blue                    "#4078f2"
  #:term-magenta                 "#a626a4"
  #:term-cyan                    "#0184bc"
  #:term-white                   "#a0a1a7"
  #:term-bright-black            "#696c77"
  #:term-bright-red              "#ca1243"
  #:term-bright-green            "#3e953a"
  #:term-bright-yellow           "#986801"
  #:term-bright-blue             "#2f5af3"
  #:term-bright-magenta          "#8a3fa0"
  #:term-bright-cyan             "#0997b3"
  #:term-bright-white            "#fafafa"
  #:syntax.keyword               "#a626a4"
  #:syntax.string                "#50a14f"
  #:syntax.comment               "#9da0a8"
  #:syntax.comment.documentation "#7c7f88"
  #:syntax.constant              "#986801"
  #:syntax.number                "#986801"
  #:syntax.function              "#4078f2"
  #:syntax.function.macro        "#0184bc"
  #:syntax.type                  "#c18401"
  #:syntax.constructor           "#c18401"
  #:syntax.attribute             "#986801"
  #:syntax.property              "#e45649"
  #:syntax.variable.builtin      "#e45649"
  #:syntax.variable.parameter    "#383a42"
  #:syntax.label                 "#e45649"
  #:syntax.escape                "#0184bc"
  #:syntax.operator              "#0184bc"
  #:syntax.string.special.key    "#e45649"
  #:syntax.punctuation           "#4f525e"
  #:syntax.boolean               "#986801"
  #:syntax.punctuation.special   "#9da0a8"
  #:syntax.text.title            "#4078f2"
  #:syntax.text.literal          "#50a14f"
  #:syntax.text.uri              "#e45649"
  #:syntax.text.reference        "#c18401"
  #:syntax.text.emphasis         "#e45649"
  #:syntax.text.strong           "#986801")

(define-theme 'solarized-dark
  #:background                   "#002b36"
  #:foreground                   "#839496"
  #:emphasis                     "#93a1a1"
  #:dim                          "#586e75"
  #:accent                       "#6c71c4"
  #:current-line                 "#073642"
  #:cursor                       "#268bd2"
  #:secondary-cursor             "#b58900"
  #:region                       "#0a435d"
  #:search-match                 "#214628"
  #:search-current               "#cb4b16"
  #:match-text                   "#2aa198"
  #:modeline                     "#073642"
  #:modeline-preview             "#1b3c59"
  #:panel                        "#073642"
  #:panel-border                 "#586e75"
  #:row-selected                 "#0d475f"
  #:row-hover                    "#002b36"
  #:badge                        "#002b36"
  #:overlay                      "#002b3699"
  #:term-black                   "#073642"
  #:term-red                     "#dc322f"
  #:term-green                   "#859900"
  #:term-yellow                  "#b58900"
  #:term-blue                    "#268bd2"
  #:term-magenta                 "#d33682"
  #:term-cyan                    "#2aa198"
  #:term-white                   "#eee8d5"
  #:term-bright-black            "#002b36"
  #:term-bright-red              "#cb4b16"
  #:term-bright-green            "#586e75"
  #:term-bright-yellow           "#657b83"
  #:term-bright-blue             "#839496"
  #:term-bright-magenta          "#6c71c4"
  #:term-bright-cyan             "#93a1a1"
  #:term-bright-white            "#fdf6e3"
  #:syntax.keyword               "#859900"
  #:syntax.string                "#2aa198"
  #:syntax.comment               "#586e75"
  #:syntax.comment.documentation "#657b83"
  #:syntax.constant              "#d33682"
  #:syntax.number                "#d33682"
  #:syntax.function              "#268bd2"
  #:syntax.function.macro        "#6c71c4"
  #:syntax.type                  "#b58900"
  #:syntax.constructor           "#b58900"
  #:syntax.attribute             "#6c71c4"
  #:syntax.property              "#268bd2"
  #:syntax.variable.builtin      "#cb4b16"
  #:syntax.variable.parameter    "#839496"
  #:syntax.label                 "#cb4b16"
  #:syntax.escape                "#dc322f"
  #:syntax.operator              "#859900"
  #:syntax.string.special.key    "#268bd2"
  #:syntax.punctuation           "#839496"
  #:syntax.boolean               "#d33682"
  #:syntax.punctuation.special   "#586e75"
  #:syntax.text.title            "#268bd2"
  #:syntax.text.literal          "#2aa198"
  #:syntax.text.uri              "#268bd2"
  #:syntax.text.reference        "#b58900"
  #:syntax.text.emphasis         "#cb4b16"
  #:syntax.text.strong           "#d33682")

(define-theme 'solarized-light
  #:background                   "#fdf6e3"
  #:foreground                   "#657b83"
  #:emphasis                     "#586e75"
  #:dim                          "#93a1a1"
  #:accent                       "#6c71c4"
  #:current-line                 "#eee8d5"
  #:cursor                       "#268bd2"
  #:secondary-cursor             "#b58900"
  #:region                       "#c7dbdf"
  #:search-match                 "#dfdfaa"
  #:search-current               "#cb4b16"
  #:match-text                   "#2aa198"
  #:modeline                     "#eee8d5"
  #:modeline-preview             "#dad6d2"
  #:panel                        "#eee8d5"
  #:panel-border                 "#93a1a1"
  #:row-selected                 "#c6d5d4"
  #:row-hover                    "#fdf6e3"
  #:badge                        "#fdf6e3"
  #:overlay                      "#002b3626"
  #:term-black                   "#073642"
  #:term-red                     "#dc322f"
  #:term-green                   "#859900"
  #:term-yellow                  "#b58900"
  #:term-blue                    "#268bd2"
  #:term-magenta                 "#d33682"
  #:term-cyan                    "#2aa198"
  #:term-white                   "#eee8d5"
  #:term-bright-black            "#002b36"
  #:term-bright-red              "#cb4b16"
  #:term-bright-green            "#586e75"
  #:term-bright-yellow           "#657b83"
  #:term-bright-blue             "#839496"
  #:term-bright-magenta          "#6c71c4"
  #:term-bright-cyan             "#93a1a1"
  #:term-bright-white            "#fdf6e3"
  #:syntax.keyword               "#859900"
  #:syntax.string                "#2aa198"
  #:syntax.comment               "#93a1a1"
  #:syntax.comment.documentation "#839496"
  #:syntax.constant              "#d33682"
  #:syntax.number                "#d33682"
  #:syntax.function              "#268bd2"
  #:syntax.function.macro        "#6c71c4"
  #:syntax.type                  "#b58900"
  #:syntax.constructor           "#b58900"
  #:syntax.attribute             "#6c71c4"
  #:syntax.property              "#268bd2"
  #:syntax.variable.builtin      "#cb4b16"
  #:syntax.variable.parameter    "#657b83"
  #:syntax.label                 "#cb4b16"
  #:syntax.escape                "#dc322f"
  #:syntax.operator              "#859900"
  #:syntax.string.special.key    "#268bd2"
  #:syntax.punctuation           "#657b83"
  #:syntax.boolean               "#d33682"
  #:syntax.punctuation.special   "#93a1a1"
  #:syntax.text.title            "#268bd2"
  #:syntax.text.literal          "#2aa198"
  #:syntax.text.uri              "#268bd2"
  #:syntax.text.reference        "#b58900"
  #:syntax.text.emphasis         "#cb4b16"
  #:syntax.text.strong           "#d33682")

;;; Major and minor modes

(define-major-mode 'fundamental-mode
  #:lighter "Fundamental")

(define-major-mode 'text-mode
  #:lighter "Text"
  #:files '("*.txt" "*.text" "README" "LICENSE" "COPYING")
  #:minor-modes '(auto-fill-mode))

(define-major-mode 'markdown-mode
  #:lighter "Markdown"
  #:parent 'text-mode
  #:files '("*.md" "*.markdown")
  #:syntax 'markdown)

(define-major-mode 'prog-mode
  #:lighter "Prog"
  #:minor-modes '(electric-pair-mode))

(define-major-mode 'rust-mode
  #:lighter "Rust"
  #:parent 'prog-mode
  #:files '("*.rs")
  #:interpreters '("rust-script")
  #:syntax 'rust
  #:fill-column 100
  #:tab-width 4)

(define-major-mode 'json-mode
  #:lighter "JSON"
  #:parent 'prog-mode
  #:files '("*.json" "*.jsonc" ".prettierrc" ".eslintrc")
  #:syntax 'json
  #:tab-width 2)

(define-major-mode 'toml-mode
  #:lighter "TOML"
  #:parent 'prog-mode
  #:files '("*.toml" "Cargo.lock" "Pipfile" "poetry.lock")
  #:syntax 'toml
  #:tab-width 2)

(define-major-mode 'yaml-mode
  #:lighter "YAML"
  #:parent 'prog-mode
  #:files '("*.yaml" "*.yml" ".clang-format")
  #:syntax 'yaml
  #:tab-width 2)

(define-major-mode 'sh-mode
  #:lighter "Shell"
  #:parent 'prog-mode
  #:files '("*.sh" "*.bash" "*.zsh" ".bashrc" ".bash_profile" ".profile" ".zshrc" ".zprofile" ".zshenv" "PKGBUILD")
  #:interpreters '("sh" "bash" "zsh" "dash" "ksh")
  #:syntax 'bash
  #:tab-width 2)

(define-major-mode 'python-mode
  #:lighter "Python"
  #:parent 'prog-mode
  #:files '("*.py" "*.pyi" "*.pyw" "SConstruct" "SConscript")
  #:interpreters '("python" "python3" "python2")
  #:syntax 'python
  #:fill-column 88
  #:tab-width 4)

(define-major-mode 'scheme-mode
  #:lighter "Scheme"
  #:parent 'lisp-mode
  #:files '("*.scm" "*.ss" "*.sld" "*.sls" "*.rkt")
  #:interpreters '("guile" "racket" "chez" "scheme" "steel")
  #:syntax 'scheme)

(define-major-mode 'emacs-lisp-mode
  #:lighter "ELisp"
  #:parent 'lisp-mode
  #:files '("*.el" ".emacs" "_emacs")
  #:interpreters '("emacs")
  #:syntax 'elisp)

(define-major-mode 'dired-mode
  #:lighter "Dired")

(define-major-mode 'lisp-mode
  #:lighter "Lisp"
  #:parent 'prog-mode
  #:files '("*.lisp" "*.lsp" "*.cl" "*.clj" "*.edn")
  #:interpreters '("sbcl" "clisp")
  #:tab-width 2)

(define-minor-mode 'auto-fill-mode #:lighter "Fill")
(define-minor-mode 'read-only-mode #:lighter "RO")
(define-minor-mode 'overwrite-mode #:lighter "Ovwrt")
(define-minor-mode 'electric-pair-mode #:lighter "")

(define-keys 'dired-mode
  "RET"     'dired-find-file
  "f"       'dired-find-file
  "e"       'dired-find-file
  "o"       'dired-find-file-other-window
  "^"       'dired-up-directory
  "C-x C-j" 'dired-up-directory
  "n"       'dired-next-line
  "SPC"     'dired-next-line
  "C-n"     'dired-next-line
  "<down>"  'dired-next-line
  "p"       'dired-previous-line
  "C-p"     'dired-previous-line
  "<up>"    'dired-previous-line
  "m"       'dired-mark
  "u"       'dired-unmark
  "DEL"     'dired-unmark-backward
  "S-u"     'dired-unmark-all-marks
  "t"       'dired-toggle-marks
  "d"       'dired-flag-file-deletion
  "x"       'dired-do-flagged-delete
  "S-d"     'dired-do-delete
  "S-r"     'dired-do-rename
  "S-c"     'dired-do-copy
  "+"       'dired-create-directory
  "g"       'dired-revert
  "s"       'dired-sort-toggle
  "w"       'dired-copy-filename-as-kill
  "q"       'quit-window)

;;; Keymaps

(define-keys 'global
  "C-f"           'forward-char
  "<right>"       'forward-char
  "C-b"           'backward-char
  "<left>"        'backward-char
  "M-f"           'forward-word
  "C-<right>"     'forward-word
  "M-<right>"     'forward-word
  "M-b"           'backward-word
  "C-<left>"      'backward-word
  "M-<left>"      'backward-word
  "C-n"           'next-line
  "<down>"        'next-line
  "C-p"           'previous-line
  "<up>"          'previous-line
  "C-a"           'move-beginning-of-line
  "<home>"        'move-beginning-of-line
  "s-<left>"      'move-beginning-of-line
  "C-e"           'move-end-of-line
  "<end>"         'move-end-of-line
  "s-<right>"     'move-end-of-line
  "M-m"           'back-to-indentation
  "M-a"           'backward-sentence
  "M-e"           'forward-sentence
  "M-{"           'backward-paragraph
  "C-<up>"        'backward-paragraph
  "M-}"           'forward-paragraph
  "C-<down>"      'forward-paragraph
  "C-M-f"         'forward-sexp
  "C-M-<right>"   'forward-sexp
  "C-M-b"         'backward-sexp
  "C-M-<left>"    'backward-sexp
  "C-M-u"         'backward-up-list
  "C-M-<up>"      'backward-up-list
  "C-M-d"         'down-list
  "C-M-<down>"    'down-list
  "M-<"           'beginning-of-buffer
  "C-<home>"      'beginning-of-buffer
  "s-<up>"        'beginning-of-buffer
  "M->"           'end-of-buffer
  "C-<end>"       'end-of-buffer
  "s-<down>"      'end-of-buffer
  "C-v"           'scroll-up-command
  "<next>"        'scroll-up-command
  "M-v"           'scroll-down-command
  "<prior>"       'scroll-down-command
  "M-g g"         'goto-line
  "M-g M-g"       'goto-line
  "C-l"           'recenter-top-bottom

  "C-SPC"         'set-mark-command
  "C-@"           'set-mark-command
  "C-x C-x"       'exchange-point-and-mark
  "C-x h"         'mark-whole-buffer
  "s-a"           'mark-whole-buffer
  "M-@"           'mark-word
  "M-h"           'mark-paragraph
  "C-M-SPC"       'mark-sexp
  "C-M-@"         'mark-sexp
  "C-g"           'keyboard-quit
  "ESC"           'keyboard-quit

  "C-k"           'kill-line
  "C-S-DEL"       'kill-whole-line
  "C-w"           'kill-region
  "s-x"           'kill-region
  "M-w"           'kill-ring-save
  "s-c"           'kill-ring-save
  "M-d"           'kill-word
  "C-<delete>"    'kill-word
  "M-DEL"         'backward-kill-word
  "C-DEL"         'backward-kill-word
  "M-k"           'kill-sentence
  "C-x DEL"       'backward-kill-sentence
  "C-M-k"         'kill-sexp
  "C-M-DEL"       'backward-kill-sexp
  "M-z"           'zap-to-char
  "C-y"           'yank
  "s-v"           'yank
  "M-y"           'yank-pop

  "C-d"           'delete-char
  "<delete>"      'delete-char
  "DEL"           'delete-backward-char
  "M-\\"          'delete-horizontal-space
  "M-SPC"         'just-one-space
  "M-^"           'delete-indentation
  "C-x C-o"       'delete-blank-lines

  "RET"           'newline
  "C-j"           'electric-newline-and-maybe-indent
  "C-o"           'open-line
  "TAB"           'indent-for-tab-command
  "C-x TAB"       'indent-rigidly
  "C-q"           'quoted-insert

  "C-t"           'transpose-chars
  "M-t"           'transpose-words
  "C-x C-t"       'transpose-lines
  "C-M-t"         'transpose-sexps
  "M-u"           'upcase-word
  "M-l"           'downcase-word
  "M-c"           'capitalize-word
  "C-x C-u"       'upcase-region
  "C-x C-l"       'downcase-region

  "M-q"           'fill-paragraph
  "C-x f"         'set-fill-column

  "C-/"           'undo
  "C-_"           'undo
  "C-x u"         'undo
  "s-z"           'undo
  "C-?"           'undo-redo
  "C-M-_"         'undo-redo
  "S-s-z"         'undo-redo

  "C->"           'mc/mark-next-like-this
  "C-<"           'mc/mark-previous-like-this
  "C-c C-<"       'mc/mark-all-like-this
  "C-c C->"       'mc/skip-to-next-like-this
  "C-c C-."       'mc/unmark-next-like-this
  "C-S-c C-S-c"   'mc/edit-lines
  "s-d"           'mc/mark-next-like-this

  "C-s"           'swiper
  "s-f"           'swiper
  "C-r"           'swiper-backward

  "C-x b"         'switch-to-buffer
  "C-x C-b"       'list-buffers
  "C-x <left>"    'previous-buffer
  "C-x <right>"   'next-buffer
  "C-x k"         'kill-buffer

  "C-x C-f"       'find-file
  "C-x d"         'dired
  "C-x C-j"       'dired-jump
  "C-x C-s"       'save-buffer
  "s-s"           'save-buffer
  "C-x C-c"       'save-buffers-kill-terminal
  "s-q"           'save-buffers-kill-terminal
  "M-x"           'execute-extended-command
  "s-,"           'open-settings

  "C-x 2"         'split-window-below
  "C-x 3"         'split-window-right
  "C-x 0"         'delete-window
  "C-x 1"         'delete-other-windows
  "C-x o"         'other-window
  "C-x +"         'balance-windows
  "C-x ^"         'enlarge-window
  "C-x }"         'enlarge-window-horizontally
  "C-x {"         'shrink-window-horizontally
  "C-x w <left>"  'windmove-left
  "C-x w <right>" 'windmove-right
  "C-x w <up>"    'windmove-up
  "C-x w <down>"  'windmove-down
  "C-x t 2"       'tab-new
  "C-x t 0"       'tab-close
  "C-x t o"       'tab-next
  "C-x t S-o"     'tab-previous
  "C-x t r"       'tab-rename
  "C-TAB"         'tab-next
  "C-S-TAB"       'tab-previous

  "C-x C-q"       'read-only-mode
  "<insert>"      'overwrite-mode

  "C-u"           'universal-argument
  "M--"           'negative-argument
  "C--"           'negative-argument
  "C-M--"         'negative-argument
  "M-0"           'digit-argument
  "M-1"           'digit-argument
  "M-2"           'digit-argument
  "M-3"           'digit-argument
  "M-4"           'digit-argument
  "M-5"           'digit-argument
  "M-6"           'digit-argument
  "M-7"           'digit-argument
  "M-8"           'digit-argument
  "M-9"           'digit-argument
  "C-0"           'digit-argument
  "C-1"           'digit-argument
  "C-2"           'digit-argument
  "C-3"           'digit-argument
  "C-4"           'digit-argument
  "C-5"           'digit-argument
  "C-6"           'digit-argument
  "C-7"           'digit-argument
  "C-8"           'digit-argument
  "C-9"           'digit-argument
  "C-M-0"         'digit-argument
  "C-M-1"         'digit-argument
  "C-M-2"         'digit-argument
  "C-M-3"         'digit-argument
  "C-M-4"         'digit-argument
  "C-M-5"         'digit-argument
  "C-M-6"         'digit-argument
  "C-M-7"         'digit-argument
  "C-M-8"         'digit-argument
  "C-M-9"         'digit-argument)

(define-keys 'picker
  "C-g"     'picker-quit
  "ESC"     'picker-quit
  "RET"     'picker-accept
  "C-j"     'picker-accept-input
  "M-RET"   'picker-accept-input
  "C-n"     'picker-next
  "<down>"  'picker-next
  "TAB"     'picker-complete
  "C-p"     'picker-previous
  "<up>"    'picker-previous
  "S-TAB"   'picker-previous
  "C-v"     'picker-page-down
  "<next>"  'picker-page-down
  "M-v"     'picker-page-up
  "<prior>" 'picker-page-up
  "M-<"     'picker-first
  "M->"     'picker-last
  "DEL"     'picker-delete-char
  "M-DEL"   'picker-delete-word
  "C-w"     'picker-delete-word
  "C-k"     'picker-kill-buffer)

(define-keys 'settings
  "C-g"     'settings-quit
  "ESC"     'settings-quit
  "s-,"     'settings-close
  "s-w"     'settings-close
  "RET"     'settings-accept
  "C-n"     'settings-next
  "<down>"  'settings-next
  "C-p"     'settings-previous
  "<up>"    'settings-previous
  "TAB"     'settings-next-section
  "S-TAB"   'settings-previous-section
  "C-v"     'settings-page-down
  "<next>"  'settings-page-down
  "M-v"     'settings-page-up
  "<prior>" 'settings-page-up
  "M-<"     'settings-first
  "M->"     'settings-last
  "DEL"     'settings-delete-char
  "M-DEL"   'settings-delete-word
  "C-w"     'settings-delete-word
  "C-u"     'settings-clear
  "C-k"     'settings-reset
  "C-o"     'settings-open-init-file)

(define-keys 'terminal
  "C-c C-j"   'term-copy-mode
  "C-c C-k"   'term-char-mode
  "C-c C-c"   'term-interrupt-subjob
  "C-c C-d"   'term-send-eof
  "C-c C-z"   'term-stop-subjob
  "C-c C-\\"  'term-quit-subjob
  "C-c C-y"   'term-paste
  "s-v"       'term-paste
  "S-<prior>" 'term-scroll-up
  "S-<next>"  'term-scroll-down
  "C-TAB"     'tab-next
  "C-S-TAB"   'tab-previous)

(define-keys 'search
  "C-g"     'search-quit
  "ESC"     'search-quit
  "RET"     'search-accept
  "C-s"     'search-next-or-history
  "s-f"     'search-next-or-history
  "s-g"     'search-next-or-history
  "C-r"     'search-previous-or-history
  "S-s-g"   'search-previous-or-history
  "C-n"     'search-next
  "<down>"  'search-next
  "TAB"     'search-next
  "C-p"     'search-previous
  "<up>"    'search-previous
  "S-TAB"   'search-previous
  "C-v"     'search-page-down
  "<next>"  'search-page-down
  "M-v"     'search-page-up
  "<prior>" 'search-page-up
  "M-<"     'search-first
  "M->"     'search-last
  "DEL"     'search-delete-char
  "M-DEL"   'search-delete-word
  "C-w"     'search-delete-word
  "C-u"     'search-clear
  "M-n"     'search-insert-symbol)
