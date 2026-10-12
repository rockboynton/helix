;; Personal feature-branch configuration. The regular config.toml is untouched.
;; Migrated from ~/.config/helix/config.toml; launch with contrib/hx-personal.
(require "helix/configuration.scm")
(require "helix/keymaps.scm")
(require (prefix-in hx. "helix/commands.scm"))

;; Editor settings.
(set-option! "mouse" #f)
(set-option! "true-color" #t)
(set-option! "line-number" "relative")
(set-option! "rulers" (list 120))
(set-option! "color-modes" #t)
(set-option! "text-width" 120)
(set-option! "cursorline" #t)
(set-option! "shell" (list "fish" "-c"))
(set-option! "jump-label-alphabet" "asdf;lkjghqweruioptyzxc/.,mbn")
(set-option! "bufferline" "multiple")
(set-option! "completion-trigger-len" 1)
(set-option! "completion-timeout" 100)
(set-option! "undercurl" #t)
(set-option! "end-of-line-diagnostics" "hint")
(set-option! "rainbow-brackets" #t)
(set-option! "file-picker.hidden" #f)
(set-option! "auto-reload" #t)
(set-option! "enable-steel" #t)
(set-option! "inline-blame.show" "never")
(set-option! "inline-blame.auto-fetch" #t)
(set-option! "inline-diagnostics.cursor-line" "warning")
(set-option! "auto-save.focus-lost" #t)
(set-option! "auto-save.after-delay.enable" #t)
(set-option! "auto-save.after-delay.timeout" 300)
(set-option! "cursor-shape.insert" "bar")
(set-option! "cursor-shape.normal" "block")
(set-option! "cursor-shape.select" "underline")
(set-option! "statusline.left" (list "mode" "spinner" "file-type-icon" "file-name" "read-only-indicator" "file-modification-indicator" "version-control"))
(set-option! "statusline.center" (list "diagnostics" "spacer" "workspace-diagnostics"))
(set-option! "statusline.right" (list "selections" "register" "search-position" "position" "position-percentage" "file-encoding"))
(set-option! "statusline.mode.normal" "NORMAL")
(set-option! "statusline.mode.insert" "INSERT")
(set-option! "statusline.mode.select" "SELECT")
(set-option! "lsp.display-messages" #t)
(set-option! "lsp.display-inlay-hints" #t)
(set-option! "whitespace.render" (hash "space" "all" "tab" "all"))
(set-option! "indent-guides.render" #t)
(set-option! "indent-guides.skip-levels" 1)
(set-option! "indent-guides.character" "┊")

;; Apply visuals after true-color and the other editor options.
(hx.theme "modified_gruvbox")
(hx.icons "nerdfonts")

;; Preserve your normal, insert, and select bindings, merging into defaults.
(add-global-keybinding
  (hash
    "normal" (hash
      "tab" "move_parent_node_end"
      "S-tab" "move_parent_node_start"
      "C-u" (list "page_cursor_half_up" "align_view_center")
      "C-d" (list "page_cursor_half_down" "align_view_center")
      "A-y" (list ":yank-join")
      "A-o" "jump_backward"
      "A-i" "jump_forward"
      "A-c" "copy_selection_on_prev_line"
      "A-H" "goto_previous_buffer"
      "A-L" "goto_next_buffer"
      "}" "goto_next_paragraph"
      "{" "goto_prev_paragraph"
      "X" "extend_line_above"
      "D" (list "move_char_left" "delete_selection")
      "H" "jump_view_left"
      "L" "jump_view_right"
      "A-f" ":format"
      "A-r" ":reload-all"
      "A-R" ":reflow"
      "A-W" "move_next_sub_word_start"
      "A-B" "move_prev_sub_word_start"
      "A-E" "move_next_sub_word_end"
      "ret" "goto_word"
      "C-S" "save_selection"
      "space" (hash
        "x" ":buffer-close"
        "X" ":buffer-close-others"
        "A-f" "file_picker_in_current_buffer_directory"
        "A-y" ":clipboard-yank-join"
        "." ":toggle-option file-picker.git-ignore"
        "n" ":new"
        "l" ":lsp-restart"
        "q" ":quit-all"
        "i" ":toggle lsp.display-inlay-hints"
        "t" ":toggle soft-wrap.enable"
        "E" "file_explorer_in_current_buffer_directory"
        "K" "goto_hover"
        "'" "select_register_history")
      "'" (hash
        "y" (list ":sh rm -f /tmp/unique-ca1ea106" ":insert-output yazi \"%{buffer_name}\" --chooser-file=/tmp/unique-ca1ea106" ":sh printf \"\\x1b[?1049h\\x1b[?2004h\" > /dev/tty" ":open %sh{cat /tmp/unique-ca1ea106}" ":redraw" ":set mouse false" ":set mouse true")
        "r" ":reset-diff-change"
        "g" (hash
          "t" (list ":sh zellij run --floating -x 5%% -y 6%% --height 88%% --width 91%% --close-on-exit -- jjui" ":reload-all")
          "b" ":sh zellij run --floating -x 5%% -y 6%% --height 88%% --width 91%% -n 'Annotate' -- jj file annotate '%{buffer_name}'"
          "u" ":sh gh browse -n '%{buffer_name}:%{selection_line_start}-%{selection_line_end}' | pbcopy"
          "o" ":sh gh browse '%{buffer_name}:%{selection_line_start}-%{selection_line_end}'"
          "d" (hash
            "i" ":sh zellij run --floating -x 5%% -y 6%% --height 88%% --width 91%% -n 'Diff (Inline)' -- jj diff --tool delta"
            "s" ":sh zellij run --floating -x 5%% -y 6%% --height 88%% --width 91%% -n 'Diff (Split)' -- jj diff")
          "c" ":sh zellij action write-chars '/<<<<<<<' && zellij action write 13")
        "'" ":sh zellij action new-pane --floating -x 5% -y 6% --height 88% --width 91%"))
    "insert" (hash
      "S-tab" "move_parent_node_start"
      "A-left" "move_prev_word_start"
      "A-right" (list "move_next_word_end" "move_char_right")
      "C-left" "move_prev_word_start"
      "C-right" (list "move_next_word_end" "move_char_right")
      "A-p" "command_palette")
    "select" (hash
      "tab" "extend_parent_node_end"
      "S-tab" "extend_parent_node_start"
      "X" "select_line_above"
      "A-W" "extend_next_sub_word_start"
      "A-B" "extend_prev_sub_word_start"
      "A-E" "extend_next_sub_word_end")))

