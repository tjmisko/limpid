-- limpid's editor init. Executed once through nvim_exec_lua after the
-- user's own configuration (in "user" mode) or on a --clean nvim (in
-- "clean" mode), with the rpc channel id as the single argument. It runs
-- last, so its options and buffer-local maps win over the user's.

local chan = ...

local limpid = { chan = chan, buf = nil }
_G.limpid = limpid

-- Presentation: limpid draws the command line and messages itself.
vim.o.laststatus = 0
vim.o.showmode = false
vim.o.ruler = false
vim.o.showcmd = false
vim.o.more = false
vim.o.shortmess = vim.o.shortmess .. "I"

local function style_window(win)
  vim.wo[win].number = false
  vim.wo[win].relativenumber = false
  vim.wo[win].signcolumn = "no"
  vim.wo[win].foldcolumn = "0"
  vim.wo[win].wrap = true
  vim.wo[win].linebreak = true
  vim.wo[win].spell = false
  vim.wo[win].cursorline = false
  vim.wo[win].colorcolumn = ""
end

local function buffer_text(buf)
  return table.concat(vim.api.nvim_buf_get_lines(buf, 0, -1, false), "\n")
end

-- Replace the current buffer with a fresh acwrite buffer holding `text`.
-- Empty text starts in insert mode; otherwise the cursor lands at the end
-- in normal mode.
function limpid.open(text, kind)
  local previous = limpid.buf
  local buf = vim.api.nvim_create_buf(true, true)
  limpid.buf = buf
  vim.bo[buf].bufhidden = "wipe"
  vim.bo[buf].swapfile = false
  vim.bo[buf].buftype = "acwrite"
  vim.bo[buf].filetype = "markdown"
  vim.api.nvim_buf_set_name(buf, "limpid://" .. kind)
  vim.b[buf].limpid_kind = kind
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, vim.split(text, "\n", { plain = true }))
  vim.bo[buf].modified = false
  vim.api.nvim_win_set_buf(0, buf)
  style_window(0)
  if previous and previous ~= buf and vim.api.nvim_buf_is_valid(previous) then
    pcall(vim.api.nvim_buf_delete, previous, { force = true })
  end

  vim.api.nvim_create_autocmd("BufWriteCmd", {
    buffer = buf,
    callback = function()
      vim.rpcnotify(chan, "limpid", "save", buffer_text(buf))
      vim.bo[buf].modified = false
    end,
  })
  vim.keymap.set({ "n", "i" }, "<C-CR>", "<Cmd>wq<CR>", { buffer = buf, silent = true })

  if text == "" then
    vim.cmd("startinsert")
  else
    vim.cmd("normal! G$")
  end
  return true
end

-- Leaving nvim ends the box. A buffer still modified at this point was
-- abandoned (:q!), one that was saved is not.
vim.api.nvim_create_autocmd("VimLeavePre", {
  callback = function()
    local discard = false
    if limpid.buf and vim.api.nvim_buf_is_valid(limpid.buf) then
      discard = vim.bo[limpid.buf].modified
    end
    vim.rpcnotify(chan, "limpid", "quit", discard)
  end,
})

return true
