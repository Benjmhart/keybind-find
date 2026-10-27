-- Synthetic config for the nvim provider's tests (story 1.4). Loaded with `-u <this> --noplugin -i NONE`.
vim.g.mapleader = " "
vim.keymap.set("n", "<leader>ff", ":echo 1<CR>", { desc = "Find files" })
vim.keymap.set("n", "<C-s>", "<cmd>w<CR>", { desc = "Save" })
vim.keymap.set("x", "<M-j>", ":m '>+1<CR>", { desc = "Move down" })
vim.keymap.set("i", "jk", "<Esc>")
vim.keymap.set("n", "<leader>x", function() end, { desc = "Callback map" })
vim.keymap.set("n", "<Plug>(kbf-test)", ":echo 2<CR>")

-- Mimic lazy.nvim: it fires `User VeryLazy` (where LazyVim and user keymaps.lua add maps) only after UIEnter, which
-- headless nvim never sends; the provider sends UIEnter and waits for `vim.g.did_very_lazy`.
package.loaded.lazy = package.loaded.lazy or {}
vim.api.nvim_create_autocmd("UIEnter", {
  callback = function()
    vim.schedule(function()
      vim.g.did_very_lazy = true
      vim.api.nvim_exec_autocmds("User", { pattern = "VeryLazy" })
    end)
  end,
})
vim.api.nvim_create_autocmd("User", {
  pattern = "VeryLazy",
  callback = function()
    vim.keymap.set("n", "<leader>w", "<cmd>w<CR>", { desc = "Late map" })
  end,
})
