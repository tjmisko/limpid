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

local function float_config()
  return {
    relative = "editor",
    row = 0,
    col = 0,
    width = math.max(1, vim.o.columns),
    height = math.max(1, vim.o.lines - vim.o.cmdheight),
    style = "minimal",
    -- limpid draws the box chrome; never inherit the user's winborder.
    border = "none",
    zindex = 50,
  }
end

-- Close the box window without reporting a quit (a new box replaces it).
local function close_silently()
  local win = limpid.win
  limpid.win = nil
  if win and vim.api.nvim_win_is_valid(win) then
    pcall(vim.api.nvim_win_close, win, true)
  end
end

-- Open a fresh acwrite buffer holding `text` in a full-grid floating window
-- over the base window. The nvim process outlives the box: closing the
-- window (:q, :wq, :q!, Ctrl-Enter) reports a quit and nothing more.
-- Empty text starts in insert mode; otherwise the cursor lands at the end
-- in normal mode.
function limpid.open(text, kind)
  close_silently()
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

  local win = vim.api.nvim_open_win(buf, true, float_config())
  limpid.win = win
  style_window(win)
  -- Absent before nvim 0.10; the box is then only protected from focus loss.
  pcall(function()
    vim.wo[win].winfixbuf = true
  end)
  if previous and previous ~= buf and vim.api.nvim_buf_is_valid(previous) then
    pcall(vim.api.nvim_buf_delete, previous, { force = true })
  end

  -- The box ends when its window closes. A buffer still modified at this
  -- point was abandoned (:q!); one that was saved was not.
  vim.api.nvim_create_autocmd("WinClosed", {
    pattern = tostring(win),
    once = true,
    callback = function()
      if limpid.win ~= win then
        return
      end
      limpid.win = nil
      local discard = vim.api.nvim_buf_is_valid(buf) and vim.bo[buf].modified
      vim.rpcnotify(chan, "limpid", "quit", discard and true or false)
    end,
  })
  vim.keymap.set({ "n", "i" }, "<C-CR>", "<Cmd>wq<CR>", { buffer = buf, silent = true })

  if text == "" then
    vim.cmd("startinsert")
  else
    vim.cmd("stopinsert")
    vim.cmd("normal! G$")
  end
  return true
end

-- Writes are intercepted for every buffer, so no :w ever reaches a file. Only
-- the box buffer reports a save; any other buffer is refused.
vim.api.nvim_create_autocmd("BufWriteCmd", {
  callback = function(args)
    if args.buf ~= limpid.buf then
      vim.notify("limpid: only the box can be written", vim.log.levels.ERROR)
      return
    end
    vim.rpcnotify(chan, "limpid", "save", buffer_text(args.buf))
    vim.bo[args.buf].modified = false
  end,
})

-- Lock the box: while it is open, focus cannot leave it (<C-w>w) and its
-- window cannot show another buffer (:enew, :e; 'winfixbuf' on the window,
-- set in limpid.open), or the user would be stranded somewhere that cannot
-- save.
local function box_is_open()
  return limpid.win ~= nil and vim.api.nvim_win_is_valid(limpid.win)
end

vim.api.nvim_create_autocmd("WinEnter", {
  callback = function()
    if not box_is_open() or vim.api.nvim_get_current_win() == limpid.win then
      return
    end
    pcall(vim.api.nvim_set_current_win, limpid.win)
  end,
})

-- :qa, :wqa, :xa and friends end nvim without closing the float first, so
-- report the quit from VimLeavePre too (once; the WinClosed path clears
-- limpid.win when it has already reported).
vim.api.nvim_create_autocmd("VimLeavePre", {
  callback = function()
    local win = limpid.win
    if not win then
      return
    end
    limpid.win = nil
    local buf = limpid.buf
    local discard = buf and vim.api.nvim_buf_is_valid(buf) and vim.bo[buf].modified
    vim.rpcnotify(chan, "limpid", "quit", discard and true or false)
  end,
})

-- Keep the box covering the whole grid when the UI is resized.
vim.api.nvim_create_autocmd("VimResized", {
  callback = function()
    if limpid.win and vim.api.nvim_win_is_valid(limpid.win) then
      pcall(vim.api.nvim_win_set_config, limpid.win, float_config())
    end
  end,
})

return true
