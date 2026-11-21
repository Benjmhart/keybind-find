-- demo.lua: a made-up config for keybind-find's README demo.
vim.g.mapleader = " "
vim.o.number = true
vim.o.cursorline = true
vim.o.background = "dark"
vim.cmd.colorscheme("default")

local function toggle_theme()
  vim.o.background = vim.o.background == "dark" and "light" or "dark"
end

vim.keymap.set("n", "<leader>ul", toggle_theme, { desc = "Toggle light theme" })
vim.keymap.set("n", "<leader>w", "<cmd>write<cr>", { desc = "Save file" })
vim.keymap.set("n", "<leader>q", "<cmd>quitall<cr>", { desc = "Quit all" })
vim.keymap.set("n", "<leader>ff", "<cmd>find *<cr>", { desc = "Find file" })
vim.keymap.set("n", "<leader>sg", ":vimgrep ", { desc = "Search with grep" })
vim.keymap.set("n", "<leader>bd", "<cmd>bdelete<cr>", { desc = "Delete buffer" })
vim.keymap.set("n", "<leader>ws", "<cmd>split<cr>", { desc = "Split window below" })
vim.keymap.set("n", "<leader>wv", "<cmd>vsplit<cr>", { desc = "Split window right" })
