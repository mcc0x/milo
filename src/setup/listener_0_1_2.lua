local terminal = "com.superlogical.terminal"

local function load_actions()
  local load_layout = loadfile(rex_config)
  if not load_layout then return end
  local ok, actions = pcall(load_layout, "listener")
  if ok and type(actions) == "table" then return actions end
end

local memory = load_actions()
if not memory then return end

local sessions, pending, timer, sequence = {}, {}, nil, 0

local function flush()
  if timer then timer:cancel(); timer = nil end
  local writes = {}
  for _, change in pairs(pending) do writes[#writes + 1] = change end
  pending = {}
  table.sort(writes, function(a, b) return a.sequence < b.sequence end)
  for _, change in ipairs(writes) do memory.save(change.dirs, change.before, change.after) end
end

local function resized(session_id, window_id, before, after)
  local change = pending[window_id]
  if change and change.key ~= after.key then
    flush()
    change = nil
  end
  if not change then
    local dirs = memory.directories(session_id, after.rail)
    if not dirs then return end
    change = { dirs = dirs, key = after.key, before = before.code }
    pending[window_id] = change
  end
  sequence = sequence + 1
  change.after, change.sequence = after.code, sequence
  if timer then timer:cancel() end
  timer = rex.after(0.2, function()
    timer = nil
    flush()
  end)
end

local function observe(session_id, revision)
  local last = sessions[session_id]
  if last and revision and last.revision and revision <= last.revision then return end
  local view = rex.session.view{ session_id = session_id }
  if not view then return end
  local windows = {}
  sessions[session_id] = { revision = view.revision, windows = windows }
  for _, window in ipairs(view.windows or {}) do
    local after = memory.snapshot(window)
    if after then
      windows[window.window_id] = after
      local before = last and last.windows[window.window_id]
      -- Rex sends no dedicated resize event. A changed ratio on an unchanged
      -- split tree is a resize; opening, closing, and moving panes only seed a baseline.
      if before and before.key == after.key and before.code ~= after.code then
        resized(session_id, window.window_id, before, after)
      end
    end
  end
  return view
end

local waiting = {}

-- Only consider a pristine tab with one terminal.
local function single_pane(window)
  if window.zoomed_block_id or #window.layers ~= 1 then return end
  local layer = window.layers[1]
  if layer.kind ~= "tiled" or #layer.blocks ~= 1 then return end
  local block = layer.blocks[1]
  if block.creator_name == terminal and (block.label or ""):sub(1, 5) ~= "milo:" then
    return block
  end
end

local function find_window(view, window_id)
  for _, window in ipairs(view and view.windows or {}) do
    if window.window_id == window_id then return window end
  end
end

local function forget(session_id, block_id, expected)
  local tabs = waiting[session_id]
  if not tabs or (expected and tabs[block_id] ~= expected) then return end
  tabs[block_id] = nil
  if not next(tabs) then waiting[session_id] = nil end
end

local function shell(tab)
  local process = rex.call(terminal .. ".process", tab.ctx)
  if not process then return end
  if type(process.last_exit) == "table" then return nil, true end
  local foreground = process.foreground
  local name = foreground and foreground.argv0
  if not name then return end
  if name:sub(1, 1) == "-" and foreground.path ~= "/usr/bin/login" then
    if tab.shell and tab.shell.pid ~= foreground.pid then return nil, true end
    tab.shell = foreground
    return foreground
  end
  -- On macOS the terminal starts under /usr/bin/login. Before we see the
  -- shell it can be in the foreground; once we see a shell, a command wins.
  local child = process.child
  return nil, tab.shell ~= nil or not (child and child.path == "/usr/bin/login")
end

local function arrange(tab)
  local ctx = tab.ctx
  local foreground, cancelled = shell(tab)
  if cancelled then forget(ctx.session_id, ctx.block_id, tab); return end
  local cwd = foreground and foreground.cwd
  if not cwd or tab.running then return end
  tab.running = true
  local ok, done = pcall(function()
    if not tab.apply or tab.cwd ~= cwd then
      tab.apply, tab.cwd = tab.prepare(ctx, cwd), cwd
    end
    return tab.apply(function(owner)
      -- Rex runs handlers one at a time, but the tab can change while a
      -- profile or size is being read. Revalidate just before the split.
      local current = shell(tab)
      if not current or current.cwd ~= cwd then return false end
      local view = rex.session.view{ session_id = ctx.session_id }
      local window = find_window(view, tab.window_id)
      local block = window and single_pane(window)
      local tabs = waiting[ctx.session_id]
      if not tabs or tabs[ctx.block_id] ~= tab or not block or block.block_id ~= ctx.block_id then return false end
      return true, owner and block.owner ~= owner
    end)
  end)
  tab.running = nil
  if not ok or done then forget(ctx.session_id, ctx.block_id, tab) end
end

local function new_tab(session_id, window)
  local block = single_pane(window)
  if not block then return end
  local block_id = block.block_id
  local tabs = waiting[session_id]
  if tabs and tabs[block_id] then return end
  -- Reload on creation so changing the preference doesn't require restarting
  -- the listener that remembers sizes.
  local actions = load_actions()
  if not actions or not actions.prepare_arrangement then return end
  if not tabs then tabs = {}; waiting[session_id] = tabs end
  local tab = {
    ctx = { session_id = session_id, block_id = block_id },
    window_id = window.window_id,
    prepare = actions.prepare_arrangement,
  }
  tabs[block_id] = tab
  arrange(tab)
end

local function ready(ctx, event)
  local tabs = waiting[ctx.session_id]
  local tab = tabs and tabs[event.block_id or ctx.block_id]
  if tab then arrange(tab) end
end

rex.on("terminal.process_changed", ready)
rex.on("terminal.pwd_changed", ready)
rex.on("terminal.size_changed", ready)
rex.on("block_owner_changed", ready)

local function block_gone(ctx, event)
  forget(ctx.session_id, event.block_id or ctx.block_id)
end

rex.on("block_closed", block_gone)
rex.on("terminal.child_exited", block_gone)

rex.on("session_view_changed", function(ctx, event)
  local id = ctx.session_id
  local view = observe(id, event.revision)
  local tabs = waiting[id]
  if not view or not tabs then return end
  for _, window in ipairs(view.windows or {}) do
    local block = single_pane(window)
    local tab = block and tabs[block.block_id]
    if tab and tab.window_id == window.window_id then tab.revision = view.revision end
  end
  for block, tab in pairs(tabs) do
    if tab.revision ~= view.revision then forget(id, block) end
  end
end)

rex.on("window_closed", function(ctx, event)
  for block, tab in pairs(waiting[ctx.session_id] or {}) do
    if tab.window_id == event.window_id then forget(ctx.session_id, block) end
  end
  if pending[event.window_id] then flush() end
end)

rex.on("session_destroyed", function(_, event)
  local id = event.session_id
  if id then sessions[id], waiting[id] = nil, nil end
  if next(pending) then flush() end
end)

rex.on("window_created", function(ctx, event)
  if next(pending) then flush() end
  local id = ctx.session_id
  local window = find_window(rex.session.view{ session_id = id }, event.window_id)
  if window then new_tab(id, window) end
end)

-- A new session comes with its first tab, which is not announced on its own.
rex.on("session_created", function(_, event)
  if next(pending) then flush() end
  local id = event.session and event.session.session_id
  local view = id and rex.session.view{ session_id = id }
  for _, window in ipairs(view and view.windows or {}) do new_tab(id, window) end
end)

-- `rex do --all` attaches after top-level Lua returns, so block queries
-- used by snapshot cannot establish a baseline until the listener runs.
-- `rex config check` validates handlers but has no timer scheduler.
if not rex.after then return end
rex.after(0, function()
  local listed = rex.call("session.list")
  for _, session in ipairs(listed and listed.sessions or {}) do
    local id = session.session_id
    -- A view event can already have captured this session's earlier state.
    if not sessions[id] then observe(id) end
  end
end)
