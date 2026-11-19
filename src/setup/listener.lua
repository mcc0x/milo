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
end

rex.on("session_view_changed", function(ctx, event) observe(ctx.session_id, event.revision) end)
rex.on("window_closed", function(_, event) if pending[event.window_id] then flush() end end)
rex.on("session_destroyed", function(_, event)
  if event.session_id then sessions[event.session_id] = nil end
  if next(pending) then flush() end
end)

-- A command, a floating pane, or a tab with its own layout is left alone.
local function single_pane(session_id, window_id)
  local view = rex.session.view{ session_id = session_id }
  if not view then return end
  for _, window in ipairs(view.windows or {}) do
    if window.window_id == window_id and not window.zoomed_block_id and #window.layers == 1 then
      local layer = window.layers[1]
      if layer.kind == "tiled" and #layer.blocks == 1 then
        local block = layer.blocks[1]
        if block.creator_name == terminal and (block.label or ""):sub(1, 5) ~= "milo:" then
          return block.block_id
        end
      end
    end
  end
end

local function arrange_shell(session_id, window_id, retries)
  -- Reload on creation so changing the new-tab preference does not require
  -- restarting the listener that remembers sizes.
  local actions = load_actions()
  if not actions or not actions.arrange then return end
  local block = single_pane(session_id, window_id)
  if not block then return end
  local process = rex.call(terminal .. ".process", { session_id = session_id, block_id = block })
  local foreground = process and process.foreground
  local child = process and process.child
  local name = foreground and foreground.argv0
  -- On macOS Rex first starts /usr/bin/login; it may appear as the
  -- foreground process before the login shell has taken over the terminal.
  local login_shell = name and name:sub(1, 1) == "-"
  if not name or (login_shell and not foreground.cwd)
    or (not login_shell and child and child.path == "/usr/bin/login") then
    -- Starting a shell should not hold up events for other tabs.
    if retries > 0 then
      rex.after(0.05, function() arrange_shell(session_id, window_id, retries - 1) end)
    end
    return
  end
  -- A login shell gives itself a name such as "-zsh"; a command does not.
  if not login_shell then return end
  pcall(actions.arrange, { session_id = session_id, block_id = block }, process, function(cwd)
    -- Looking up the profile can yield. A user split or a command started
    -- in the meantime must still win over automatic arrangement.
    if single_pane(session_id, window_id) ~= block then return false end
    local current = rex.call(terminal .. ".process", { session_id = session_id, block_id = block })
    local foreground = current and current.foreground
    return foreground and foreground.argv0 and foreground.argv0:sub(1, 1) == "-" and foreground.cwd == cwd
  end)
end

rex.on("window_created", function(ctx, event)
  if next(pending) then flush() end
  arrange_shell(ctx.session_id, event.window_id, 9)
end)

-- A new session comes with its first tab, which is not announced on its own.
rex.on("session_created", function(_, event)
  if next(pending) then flush() end
  local session_id = event.session and event.session.session_id
  local view = session_id and rex.session.view{ session_id = session_id }
  if not view then return end
  for _, window in ipairs(view.windows) do
    arrange_shell(session_id, window.window_id, 9)
  end
end)

-- `rex do --all` attaches after top-level Lua returns, so block queries
-- used by snapshot cannot establish a baseline until the listener runs.
rex.after(0, function()
  local listed = rex.call("session.list")
  for _, session in ipairs(listed and listed.sessions or {}) do
    local id = session.session_id
    -- A view event can already have captured this session's earlier state.
    if not sessions[id] then observe(id) end
  end
end)
