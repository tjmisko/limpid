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

-- Clipboard providers may choose an image as their default target. Inspect
-- the offered types before getreg('+') can turn those bytes into buffer text.
local function clipboard_image()
  local wayland = vim.env.WAYLAND_DISPLAY and vim.env.WAYLAND_DISPLAY ~= ""
    and vim.fn.executable("wl-paste") == 1
  local x11 = not wayland and vim.env.DISPLAY and vim.env.DISPLAY ~= ""
    and vim.fn.executable("xclip") == 1
  if not wayland and not x11 then
    return nil
  end
  local function read(args)
    if not vim.system then
      local pipe = assert(io.popen(table.concat(vim.tbl_map(vim.fn.shellescape, args), " "), "r"))
      local bytes = pipe:read("*a")
      local ok = pipe:close()
      return ok and bytes or nil
    end
    local result = vim.system(args, { text = false }):wait(5000)
    if result.code ~= 0 then
      return nil
    end
    return result.stdout
  end
  local types = read(wayland and { "wl-paste", "--list-types" }
    or { "xclip", "-selection", "clipboard", "-o", "-t", "TARGETS" })
  if not types then
    return nil
  end
  local offered = {}
  for mime in types:gmatch("[^\r\n]+") do
    offered[mime] = true
  end
  for _, format in ipairs({
    { "image/png", "png" }, { "image/jpeg", "jpg" },
    { "image/webp", "webp" }, { "image/gif", "gif" },
    { "image/bmp", "bmp" }, { "image/tiff", "tiff" },
  }) do
    local mime, extension = unpack(format)
    if offered[mime] then
      local bytes = read(wayland and { "wl-paste", "--no-newline", "--type", mime }
        or { "xclip", "-selection", "clipboard", "-o", "-t", mime })
      if not bytes or bytes == "" then
        error("could not read clipboard image (" .. mime .. ")")
      end
      local uv = vim.uv or vim.loop
      -- Neovim deletes tempname()'s directory on exit. Use an independent
      -- private directory so the link survives :wq and closing the viewer.
      local directory = assert(uv.fs_mkdtemp(uv.os_tmpdir() .. "/limpid-clipboard-XXXXXX"))
      local path = directory .. "/screenshot." .. extension
      local fd, open_error = uv.fs_open(path, "wx", 384) -- private, exclusive (0600)
      if not fd then
        uv.fs_rmdir(directory)
        error(open_error)
      end
      local written, write_error = uv.fs_write(fd, bytes, 0)
      local closed, close_error = uv.fs_close(fd)
      if written ~= #bytes or not closed then
        uv.fs_unlink(path)
        uv.fs_rmdir(directory)
        error(write_error or close_error or "incomplete clipboard image write")
      end
      -- Keep the file after the editor closes: submitted feedback links to it.
      return "[Screenshot](<" .. path .. ">)"
    end
  end
  return nil
end

-- The UI grid only contains the visible slice of the buffer (and filler
-- rows). Measure the whole buffer so wrapped and off-screen lines can grow
-- limpid's box without making its height depend on the previous grid size.
local function report_height()
  local win, buf = limpid.win, limpid.buf
  if not win or not vim.api.nvim_win_is_valid(win) then
    return
  end
  local rows
  if vim.api.nvim_win_text_height then
    local ok, height = pcall(vim.api.nvim_win_text_height, win, {
      start_row = 0,
      end_row = vim.api.nvim_buf_line_count(buf) - 1,
    })
    if ok then
      rows = height.all
    end
  end
  -- Compatibility with Neovim before nvim_win_text_height was introduced.
  if not rows then
    rows = 0
    local width = math.max(1, vim.api.nvim_win_get_width(win))
    for _, line in ipairs(vim.api.nvim_buf_get_lines(buf, 0, -1, false)) do
      rows = rows + math.max(1, math.ceil(vim.fn.strdisplaywidth(line) / width))
    end
  end
  rows = math.min(65535, math.max(1, rows))
  if limpid.rows ~= rows then
    limpid.rows = rows
    vim.rpcnotify(chan, "limpid", "content_rows", rows)
  end
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
-- window (:q, :w, :wq, :q!, Ctrl-Enter) reports a quit and nothing more.
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
  limpid.rows = nil
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
  -- Desktop paste shortcuts belong to the box even with a user's config.
  -- Use the system (+) register and the paste API so multiline text bypasses
  -- mappings and autoindent, and visual selections are replaced normally.
  for _, key in ipairs({ "<C-v>", "<C-S-v>" }) do
    vim.keymap.set({ "n", "i", "x", "s", "c" }, key, function()
      local ok, pasted = pcall(function()
        return clipboard_image() or vim.fn.getreg("+")
      end)
      if not ok then
        vim.api.nvim_err_writeln("limpid: " .. tostring(pasted))
        return
      end
      if pasted ~= "" then
        vim.api.nvim_paste(pasted, true, -1)
      end
    end, { buffer = buf, silent = true, desc = "Paste from system clipboard" })
  end

  if text == "" then
    vim.cmd("startinsert")
  else
    vim.cmd("stopinsert")
    vim.cmd("normal! G$")
  end
  report_height()
  return true
end

vim.api.nvim_create_autocmd({ "TextChanged", "TextChangedI", "TextChangedP" }, {
  callback = report_height,
})

-- Writes are intercepted for every buffer, so no :w ever reaches a file. Range
-- writes (:1w path) and appends go through other events, refused below. Only
-- the box buffer reports a save; any other buffer is refused.
vim.api.nvim_create_autocmd("BufWriteCmd", {
  callback = function(args)
    if args.buf ~= limpid.buf then
      vim.notify("limpid: only the box can be written", vim.log.levels.ERROR)
      return
    end
    vim.rpcnotify(chan, "limpid", "save", buffer_text(args.buf))
    vim.bo[args.buf].modified = false
    -- A save ends the box, so :w closes like :wq (a no-op when :wq already did).
    local win = limpid.win
    vim.schedule(function()
      if win and limpid.win == win and vim.api.nvim_win_is_valid(win) then
        pcall(vim.api.nvim_win_close, win, true)
      end
    end)
  end,
})

vim.api.nvim_create_autocmd({ "FileWriteCmd", "FileAppendCmd" }, {
  callback = function()
    vim.notify("limpid: partial writes are not allowed", vim.log.levels.ERROR)
  end,
})

-- Lock the box: while it is open, focus cannot leave it (<C-w>w) and its
-- window cannot show another buffer (:enew, :e; 'winfixbuf' on the window,
-- set in limpid.open), or the user would be stranded somewhere that cannot
-- save.
local function box_is_open()
  return limpid.win ~= nil and vim.api.nvim_win_is_valid(limpid.win)
end

-- An emptied box can be abandoned with :q even when deleting its text
-- marked it modified. Do this only at quit time so undo still restores
-- the usual protection for unsaved text.
vim.api.nvim_create_autocmd("QuitPre", {
  callback = function()
    if box_is_open()
      and vim.api.nvim_get_current_buf() == limpid.buf
      and buffer_text(limpid.buf) == ""
    then
      vim.bo[limpid.buf].modified = false
    end
  end,
})

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
      report_height()
    end
  end,
})

return true
