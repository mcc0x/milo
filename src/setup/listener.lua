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

local function find_window(view, window_id)
  for _, window in ipairs(view and view.windows or {}) do
    if window.window_id == window_id then return window end
  end
end

-- Tab titles. Milo titles a tab only while its label is Rex's own or the one
-- Milo set last, so a name anyone else chose is never replaced. Records are
-- kept by window and block ID, never by a label or a position.
local titles, titled, pane_tab, claims = memory.titles, {}, {}, nil

-- The directory and agent a tab is titled for. Only the shells of a tab that
-- is Milo's to title are ever asked about their processes.
local function tab_facts(record)
  local cwd, agent
  for _, block_id in ipairs(record.order) do
    local pane = record.panes[block_id]
    if not pane then
      pane = titles.facts(rex.call(terminal .. ".process", { session_id = record.session_id, block_id = block_id }))
      record.panes[block_id] = pane
    end
    -- The project is the agent's if one runs in this tab, else the first shell's.
    if pane.cwd and pane.agent and not agent then cwd, agent = pane.cwd, pane.agent end
    cwd = cwd or pane.cwd
  end
  return cwd, agent
end

local function release(window_id, record)
  record.claim = nil
  titles.forget{ window_id }
end

-- A label that is neither Rex's own nor the title Milo set is a name someone
-- chose, which ends Milo's part in the tab.
local function named_elsewhere(window_id, record, default)
  if record.label == default or (record.claim and record.label == record.claim) then return false end
  if record.claim then release(window_id, record) end
  return true
end

local function retitle(window_id)
  local record = titled[window_id]
  local session_id, default = record.session_id, "Window " .. window_id:sub(-8)
  if named_elsewhere(window_id, record, default) then return end
  local cwd, agent = tab_facts(record)
  if not cwd then return end
  local key = cwd .. "\n" .. (agent or "")
  local changed = key ~= record.key
  -- Recheck file-backed answers on events even when this shell has not
  -- moved, and ask again for one that is no longer kept. Answers without
  -- grounds still wait for a new cwd or agent.
  if changed or record.cached then
    local title, cached = titles.ask(cwd, agent)
    record.cached = cached
    if changed or (title ~= nil and title ~= record.title) then
      record.key, record.title, record.refused = key, title, nil
    end
  end
  local title = record.title
  if not title or record.refused then return end
  local wanted = title ~= "" and title or default
  if record.label ~= wanted then
    -- Asking Milo, Rex or a terminal takes a moment, and a name can be chosen
    -- in it. Read the label again just before replacing it.
    local window = find_window(rex.session.view{ session_id = session_id }, window_id)
    if not window then return end
    record.label = window.label
    if named_elsewhere(window_id, record, default) then return end
  end
  if record.label ~= wanted then
    if not rex.session.set_window_label{ session_id = session_id, window_id = window_id, label = title } then
      -- Don't retry the same refused title on each event.
      record.refused = true
      return
    end
    record.label = wanted
  end
  if wanted == default then
    if record.claim then release(window_id, record) end
  elseif record.claim ~= wanted then
    record.claim = wanted
    titles.claim(window_id, wanted)
  end
end

-- Stop tracking the session's tabs that are not in `seen`.
local function drop_titles(session_id, seen)
  local gone = {}
  for window_id, record in pairs(titled) do
    if record.session_id == session_id and not (seen and seen[window_id]) then
      titled[window_id] = nil
      for _, block_id in ipairs(record.order) do
        if pane_tab[block_id] == window_id then pane_tab[block_id] = nil end
      end
      if record.claim then gone[#gone + 1] = window_id end
    end
  end
  if #gone > 0 then titles.forget(gone) end
end

local function sync_titles(session_id, view)
  if not titles then return end
  claims = claims or titles.owned()
  local seen = {}
  for _, window in ipairs(view.windows or {}) do
    local window_id = window.window_id
    local record = titled[window_id]
    if not record then
      record = { session_id = session_id, panes = {}, order = {}, claim = claims[window_id] }
      titled[window_id] = record
    end
    local order, present = {}, {}
    for _, layer in ipairs(window.layers or {}) do
      for _, block in ipairs(layer.kind == "tiled" and layer.blocks or {}) do
        if block.creator_name == terminal and (block.label or ""):sub(1, 5) ~= "milo:" then
          order[#order + 1], present[block.block_id], pane_tab[block.block_id] = block.block_id, true, window_id
        end
      end
    end
    for _, block_id in ipairs(record.order) do
      if not present[block_id] then
        record.panes[block_id] = nil
        if pane_tab[block_id] == window_id then pane_tab[block_id] = nil end
      end
    end
    seen[window_id], record.label, record.order = true, window.label, order
    retitle(window_id)
  end
  drop_titles(session_id, seen)
end

local function pane_changed(ctx, event)
  local block_id = event.block_id or ctx.block_id
  local window_id = pane_tab[block_id]
  local record = window_id and titled[window_id]
  if not record then return end
  -- A process change says who is in front. After any other change the terminal is asked.
  record.panes[block_id] = event.foreground and titles.facts(event) or nil
  retitle(window_id)
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
  sync_titles(session_id, view)
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

local function forget(session_id, block_id, expected)
  local tabs = waiting[session_id]
  if not tabs or (expected and tabs[block_id] ~= expected) then return end
  tabs[block_id] = nil
  if not next(tabs) then waiting[session_id] = nil end
end

local function login_shell(process)
  local name = process.argv0
  return name and name:sub(1, 1) == "-" and process.path ~= "/usr/bin/login"
end

-- The tab's shell is the login shell its terminal started: under
-- /usr/bin/login on macOS, or as the terminal's own process. A shell copies
-- itself to run parts of its startup and its prompt, and a copy in the
-- foreground is still that shell. Rex leaves the ancestors out when one has
-- just exited, so a copy can arrive without them. A login shell that another
-- program started in the tab is not the tab's.
local function own_shell(process)
  local shell, child = process.foreground, process.child
  if not login_shell(shell) then return end
  local above
  for _, parent in ipairs(process.ancestors or {}) do
    above = parent
    if not login_shell(parent) or parent.path ~= shell.path then break end
    shell, above = parent, nil
  end
  if not child then return shell end
  if child.path == "/usr/bin/login" then
    if not above or above.pid == child.pid then return shell end
  elseif not above and login_shell(child) and child.path == shell.path then
    return shell
  end
end

local function shell(tab)
  local process = rex.call(terminal .. ".process", tab.ctx)
  if not process then return end
  if type(process.last_exit) == "table" then return nil, true end
  local foreground = process.foreground
  if not (foreground and foreground.argv0) then return end
  local own = own_shell(process)
  if own then
    tab.shell = own
    return own
  end
  local child = process.child
  -- A terminal's process is a copy of the Rex server until it becomes login,
  -- the shell or a command. A tab that is still starting has run nothing.
  if not tab.shell and child and child.pid == foreground.pid and child.name == "rex" then return end
  -- On macOS the terminal starts under /usr/bin/login. Before we see the
  -- shell it can be in the foreground; once we see a shell, a command wins.
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

rex.on("terminal.process_changed", function(ctx, event) ready(ctx, event); pane_changed(ctx, event) end)
rex.on("terminal.pwd_changed", function(ctx, event) ready(ctx, event); pane_changed(ctx, event) end)
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
  if id then
    sessions[id], waiting[id] = nil, nil
    drop_titles(id)
  end
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
  if view then sync_titles(id, view) end
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
