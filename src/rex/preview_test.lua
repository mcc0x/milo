-- With Lua 5.1: lua5.1 src/rex/preview_test.lua src/rex/preview.lua
local script = assert(arg[1])
local function id(kind, n)
  return string.format("%s:00000000-0000-0000-0000-%012x", kind, n)
end
local session, window, inactive = id("session", 1), id("window", 1), id("window", 2)
local main, sidebar, preview, other = id("block", 1), id("block", 2), id("block", 3), id("block", 4)
local app, other_app = id("client", 1), id("client", 2)
local another_session = id("session", 2)
local four_sessions = { session, another_session, id("session", 3), id("session", 4) }
local host = "00000000-0000-0000-0000-000000000001"
local term = "com.superlogical.terminal"
local exe, startup = "/with spaces/milo", "/private/tmp/milo-preview.fixture/start"
local uid, helper, cli, gui = 501, 20, 30, 40
local native_window = 4294967295
local epoch = 1790000000
local function stamp(record, when) return record .. " @" .. tostring(when or epoch) end
local show = stamp("show 40 tile 625000 0 375000 1000000")
local shown = "hide\n" .. show .. "\n"
local server = "unix:///tmp/rex%20preview.sock"
local function peer(pid) return { kind = "peer", transport = "unix", uid = uid, pid = pid } end
local function second_viewer(pid, sessions, platform, state)
  return { client_id = other_app, info = { kind = "app", platform = platform or "macos" },
    connection_state = state or "connected", principal = peer(pid), session_ids = sessions }
end
local function terminal(block, label)
  return { block_id = block, label = label, creator_name = term, host_id = host, owner = app }
end
local function float_anchor(f)
  f.tiled.blocks = { f.rail, f.main }
  f.window.layers[2] = { kind = "floating", blocks = { f.anchor } }
end
local function call(f)
  local args = f.rex.args
  local writes, callbacks = {}, {}
  local env = setmetatable({
    rex = f.rex,
    print = function(s) writes[#writes + 1] = s .. "\n" end,
    io = {
      stdout = { write = function(_, s, n) writes[#writes + 1] = s .. n end, flush = function() end },
      open = function(path, mode)
        assert(path == "/dev/fd/7" and mode == "w" and not f.ready)
        return {
          write = function(_, byte) assert(byte == "R"); f.ready = true; return true end,
          close = function() return true end,
        }
      end,
    },
    os = { time = function() return f.now or epoch end },
  }, { __index = _G })
  f.rex.on = function(event, run)
    assert(not callbacks[event], event)
    callbacks[event] = run
  end
  local after
  f.rex.after = function(delay, fn)
    if delay == 0 then
      after = fn
    elseif delay == 1 then
      callbacks.heartbeat = fn
    elseif delay == 8 then
      callbacks.deadline = fn
    else
      assert(delay == 28)
      callbacks.claim_deadline = fn
    end
  end
  f.rex.stop = function(code) f.stopped = code end
  setfenv(assert(loadfile(script)), env)()
  if after then after() end
  return function(event, payload, ctx)
    if event then assert(callbacks[event], event)(ctx or { session_id = args.session }, payload or {}) end
    return table.concat(writes)
  end
end

local function fixture(mode, block)
  local f = {
    gui = { client_id = app, info = { kind = "app", platform = "macos" },
      connection_state = "connected", principal = peer(gui), session_ids = { session } },
    cli = peer(cli),
    status = { host_id = host, listeners = { { endpoint = server, transport = "unix", admission = "peer_credentials" } } },
    main = terminal(main, "project"),
    rail = terminal(sidebar, "milo:outline"),
    anchor = terminal(preview, "milo:preview"),
    other = terminal(other, "second project"),
    referenced = main,
    shell_command = "-zsh",
    parents = { [cli] = { helper, uid, "rex" }, [helper] = { block == main and 12 or 11, uid, "milo" } },
    proc = {},
  }
  f.proc[main] = { child = { name = "zsh", path = "/bin/zsh", argv0 = "-zsh", user_id = uid, pid = 12, cwd = "/work/project" } }
  f.proc[sidebar] = { child = { name = "milo", path = exe, user_id = uid, pid = 11 } }
  f.proc[preview] = { child = { name = "milo", path = exe, user_id = uid, pid = helper } }
  f.tiled = { kind = "tiled", blocks = { f.rail, f.main, f.anchor } }
  f.leaf = { block_id = preview, rect = { 0.625, 0, 0.375, 1 } }
  f.main_leaf = { block_id = main, rect = { 0.25, 0, 0.375, 1 } }
  f.branch = { direction = "horizontal", before = f.main_leaf, after = f.leaf }
  f.tiled.layout = { rect = { 0, 0, 1, 1 }, split = {
    direction = "horizontal",
    before = { block_id = sidebar, rect = { 0, 0, 0.25, 1 } },
    after = { rect = { 0.25, 0, 0.75, 1 }, split = f.branch },
  } }
  f.window = { window_id = window, layers = { f.tiled }, focused_block_id = main }
  f.inactive = { window_id = inactive, layers = { { kind = "tiled", blocks = { f.other } } } }
  f.view = { session_id = session, active_window_id = window, revision = 1, windows = { f.window, f.inactive } }
  local function scope(a) assert(a.session_id == session) end
  local function new_split(a)
    scope(a)
    assert(not f.placed)
    f.placed = a
    f.anchor.label = a.layout.block.label
    f.branch.direction = a.direction
    f.branch.before, f.branch.after = f.main_leaf, f.leaf
    if a.side == "before" then f.branch.before, f.branch.after = f.leaf, f.main_leaf end
    if f.after_create then f.after_create() end
    return { block_ids = { preview } }
  end
  f.rex = {
    session_id = session,
    block_id = block,
    args = { mode = mode, server = server, uid = uid, pid = helper, session = session, block = block,
      exe = exe, startup = startup, tile = preview, anchor_pid = helper, placement = "right",
      main = main, window = window, cwd = "/work/project",
      owner = app, owner_pid = gui, native_window_id = native_window },
    json = { encode = function(plan) f.plan = plan; return "plan" end },
    call = function(method, a)
      if method == "server.whoami" then return { principal = f.cli } end
      if method == "server.status" then return f.status end
      if method == "client.list" then
        assert(a.session_id == nil and #a.kinds == 1 and a.kinds[1] == "app")
        return { clients = f.second_viewer and { f.second_viewer, f.gui } or { f.gui } }
      end
      if method == "block.close" then
        scope(a)
        assert(a.block_id == preview and a.keep_last == true)
        f.canceled = true
        return {}
      end
      assert(method == term .. ".process", method)
      scope(a)
      return f.proc[a.block_id]
    end,
    exec = function(argv)
      if argv[1] == "/bin/sh" then return { ok = true, stdout = tostring(cli) } end
      assert(argv[1] == "/bin/ps")
      if argv[3] == "ppid=,uid=,ucomm=" then
        local p = f.parents[tonumber(argv[5])]
        return p and { ok = true, stdout = string.format("%d %d %s", unpack(p)) } or nil
      end
      if argv[6] == "12" then return { ok = true, stdout = f.shell_command } end
      assert(argv[6] == "11")
      return { ok = true, stdout = exe .. " outline --shell " .. f.referenced .. "\n" }
    end,
    session = {
      view = function(a) scope(a); return f.view end,
      list_blocks = function(a)
        scope(a)
        local blocks = {}
        if f.detached then blocks[#blocks + 1] = { block_id = preview, placement = "detached" } end
        if f.main_detached then blocks[#blocks + 1] = { block_id = main, placement = "detached" } end
        return { blocks = blocks }
      end,
      describe_block = function(a)
        scope(a)
        assert(a.block_id == preview)
        return { block = f.anchor }
      end,
      set_block_label = function(a)
        scope(a)
        assert(a.block_id == preview and f.anchor.label == "milo:preview:starting")
        f.anchor.label, f.published = a.label, true
        return {}
      end,
      new_split = new_split,
      focus_block = function(a)
        scope(a)
        assert(not f.focused and a.block_id == main)
        f.focused = a.block_id
        if f.refuse_focus then return end
        f.window.focused_block_id = a.block_id
        return {}
      end,
    },
  }
  return f
end

local function move_preview_beside_rail(f)
  local root = f.tiled.layout.split
  local rail = root.before
  rail.rect = { 0, 0, 0.125, 1 }
  f.leaf.rect = { 0.125, 0, 0.125, 1 }
  f.main_leaf.rect = { 0.25, 0, 0.75, 1 }
  root.before = { rect = { 0, 0, 0.25, 1 }, split = { direction = "horizontal", before = rail, after = f.leaf } }
  root.after = f.main_leaf
end

for _, p in ipairs{ { "right", "horizontal", "after" }, { "left", "horizontal", "before" },
  { "above", "vertical", "before" }, { "below", "vertical", "after" } } do
  local f = fixture("open", sidebar)
  f.rex.args.placement = p[1]
  assert(call(f)() == preview .. "\n")
  local a = f.placed
  assert(a.focus == false and a.layout.block.label == "milo:preview:starting")
  assert(not f.published and f.anchor.label == "milo:preview:starting")
  local launch = a.layout.block.options
  assert(launch.shell == "none" and launch.login == false and launch.cwd == "/work/project")
  assert(launch.exit.on_completion == true and launch.exit.quick_exit_threshold_ms == 0)
  assert(#launch.command == 8 and launch.command[1] == exe and launch.command[2] == "preview-anchor"
    and launch.command[3] == startup and launch.command[4] == main and launch.command[5] == window
    and launch.command[6] == app and launch.command[7] == tostring(gui) and launch.command[8] == "4294967295")
  assert(a.anchor_id == main and a.direction == p[2] and a.side == p[3])
end

local function staged(mode, change)
  local f = fixture("open", main)
  assert(call(f)() == preview .. "\n")
  f.rex.args.mode = mode
  if change then change(f) end
  return f, call(f)()
end
local approved, authorized = staged("authorize")
assert(authorized == "ok\n" and not approved.published)
local published, sent = staged("publish")
assert(sent == "ok\n" and published.published and published.anchor.label == "milo:preview")
local forged_peer, refused_peer = staged("authorize", function(f) f.rex.args.anchor_pid = helper + 1 end)
assert(refused_peer == "changed\n" and not forged_peer.published)
local moved_peer, refused_move = staged("publish", function(f) f.view.active_window_id = inactive end)
assert(refused_move == "changed\n" and not moved_peer.published)
local canceled, canceled_reply = staged("cancel")
assert(canceled_reply == "ok\n" and canceled.canceled and not canceled.published)
local relabeled, relabeled_reply = staged("cancel", function(f) f.anchor.label = "renamed by user" end)
assert(relabeled_reply == "ok\n" and not relabeled.canceled)

local function denied(mode, block, change, status)
  local f = fixture(mode, block)
  change(f)
  assert(call(f)() == status .. "\n" and not f.placed)
end
for _, sessions in ipairs{ { session }, four_sessions } do
  local f = fixture("identify", main)
  f.gui.session_ids = sessions
  assert(call(f)() == "plan\n")
  local expected = { main = main, window = window, cwd = "/work/project", owner = app, owner_pid = gui }
  for key, value in pairs(f.plan) do assert(expected[key] == value) end
  for key, value in pairs(expected) do assert(f.plan[key] == value) end
end
for _, companion in ipairs{
  second_viewer(gui + 1, { another_session }),
  second_viewer(gui, { another_session }, "ios"),
  second_viewer(gui, { another_session }, nil, "lost"),
  second_viewer(gui, nil),
  second_viewer(gui, false),
  second_viewer(gui, {}),
  second_viewer(gui, { session }),
} do
  local f = fixture("open", main)
  f.second_viewer = companion
  assert(call(f)() == preview .. "\n")
end
for _, invalid in ipairs{ false, {}, { another_session }, { session, false }, { current = session } } do
  denied("open", main, function(f) f.gui.session_ids = invalid end, "client")
end
denied("open", main, function(f) f.gui.session_ids = nil end, "client")
local duplicates = fixture("open", main)
duplicates.gui.session_ids = { session, session }
assert(call(duplicates)() == preview .. "\n")
local attached_during_open = fixture("open", main)
attached_during_open.after_create = function() attached_during_open.gui.session_ids = { session, another_session } end
assert(call(attached_during_open)() == preview .. "\n" and not attached_during_open.canceled
  and attached_during_open.anchor.label == "milo:preview:starting"
  and #attached_during_open.placed.layout.block.options.command == 8)
local lost_during_open = fixture("open", main)
lost_during_open.after_create = function() lost_during_open.gui.session_ids = { another_session } end
assert(call(lost_during_open)() == "changed\n" and lost_during_open.canceled and not lost_during_open.published)
for _, invalid in ipairs{ false, "7", 0, -1, 0.5, 4294967296, math.huge, 0/0 } do
  denied("open", main, function(f) f.rex.args.native_window_id = invalid end, "changed")
end
denied("open", main, function(f) f.rex.args.native_window_id = nil end, "changed")
denied("open", sidebar, function(f) f.referenced = other end, "stale")
denied("open", sidebar, function(f) f.referenced = "block:0" end, "stale")
denied("open", sidebar, function(f) f.view.active_window_id = inactive end, "tab")
denied("open", main, function(f) f.rex.args.placement = "floating" end, "placement")
denied("open", main, function(f)
  f.tiled.blocks = { f.rail }
  f.inactive.layers[1].blocks = { f.other, f.main }
  f.view.active_window_id = inactive
end, "changed")
local raced = fixture("open", main)
raced.after_create = function() raced.view.active_window_id = inactive end
assert(call(raced)() == "changed\n" and raced.placed and raced.canceled and not raced.published)
for _, change in ipairs{
  move_preview_beside_rail,
  function(f) f.branch.before, f.branch.after = f.branch.after, f.branch.before end,
  function(f) f.branch.direction = "vertical" end,
} do
  local rearranged = fixture("open", main)
  rearranged.after_create = function() change(rearranged) end
  assert(call(rearranged)() == "changed\n" and rearranged.placed and rearranged.canceled and not rearranged.published)
end
local floated = fixture("open", main)
floated.after_create = function() float_anchor(floated) end
assert(call(floated)() == "changed\n" and floated.placed and floated.canceled and not floated.published)
local renamed = fixture("open", main)
renamed.after_create = function()
  renamed.view.active_window_id = inactive
  renamed.anchor.label = "renamed by user"
end
assert(call(renamed)() == "changed\n" and renamed.placed and not renamed.canceled and not renamed.published)
denied("identify", main, function(f) f.window.zoomed_block_id = main end, "zoom")
denied("open", main, function(f)
  f.tiled.blocks = { f.rail }
  f.window.layers[2] = { kind = "floating", blocks = { f.main } }
end, "floating_caller")
denied("open", sidebar, function(f) f.main.host_id = host .. "remote" end, "remote")
denied("open", main, function(f) f.proc[main].foreground = { name = "ssh" } end, "remote")
denied("open", main, function(f)
  f.proc[main].child.argv0, f.shell_command = "zsh", "zsh -ic 'echo text'"
end, "shell")
local interactive = fixture("open", main)
interactive.proc[main].child.argv0, interactive.shell_command = "zsh", "zsh -i"
assert(call(interactive)() == preview .. "\n" and interactive.placed.anchor_id == main)
denied("open", sidebar, function(f) f.gui.principal.transport = "tailscale" end, "client")
denied("open", sidebar, function(f) f.gui.info.platform = "ios" end, "client")
denied("open", sidebar, function(f) f.cli = peer(cli + 1) end, "transport")
denied("open", sidebar, function(f) f.status.listeners[1].endpoint = "unix:///tmp/elsewhere.sock" end, "transport")
denied("open", sidebar, function(f) f.rex.block_id = other end, "transport")
denied("open", sidebar, function(f) f.rex.args.cwd = "/previous/directory" end, "changed")
denied("open", sidebar, function(f) f.rex.args.owner = other_app end, "changed")
denied("open", sidebar, function(f) f.rex.args.owner_pid = gui + 1 end, "changed")
for _, mode in ipairs{ "identify", "open" } do
  denied(mode, main, function(f) f.parents[helper][1] = 11 end, "caller")
  denied(mode, sidebar, function(f) f.parents[helper][1] = 12 end, "caller")
end
local wrapped = fixture("open", main)
wrapped.parents[helper][1] = 14
wrapped.parents[14] = { 12, uid, "bash" }
wrapped.proc[main].ancestors = { wrapped.proc[main].child }
wrapped.proc[main].child = { pid = 12, user_id = 0, path = "/usr/bin/login", name = "login" }
assert(call(wrapped)() == preview .. "\n")
local nested = fixture("open", main)
local outer = nested.proc[main].child
local inner = { name = "zsh", path = "/bin/zsh", argv0 = "-zsh", user_id = uid, pid = 13, cwd = "/nested/path" }
nested.proc[main].ancestors = { inner, outer }
nested.proc[main].foreground = { name = "git" }
nested.rex.args.cwd = inner.cwd
local exec = nested.rex.exec
nested.rex.exec = function(argv)
  if argv[6] == "13" then return { ok = true, stdout = "-zsh" } end
  return exec(argv)
end
assert(call(nested)() == preview .. "\n" and nested.placed.layout.block.options.cwd == inner.cwd)
denied("open", sidebar, function(f) f.rex.args.main = other end, "changed")
denied("open", sidebar, function(f)
  f.rail.label = "milo:usage"
  f.tiled.blocks[#f.tiled.blocks + 1] = f.other
  f.inactive.layers[1].blocks = {}
  f.proc[other] = f.proc[main]
end, "shell")

-- The panel can be key without taking Rex's logical focus from the shell.
local already_focused = fixture("focus", preview)
already_focused.gui.session_ids = four_sessions
assert(call(already_focused)() == "ok\n" and not already_focused.focused)
for _, focused in ipairs{ preview, sidebar } do
  local returning = fixture("focus", preview)
  returning.window.focused_block_id = focused
  returning.window.layers[2] = { kind = "floating", state = "minimized", blocks = {} }
  assert(call(returning)() == "ok\n" and returning.focused == main and returning.window.focused_block_id == main)
end
local refused = fixture("focus", preview)
refused.window.focused_block_id, refused.refuse_focus = preview, true
assert(call(refused)() == "focus\n" and refused.focused == main and refused.window.focused_block_id == preview)

for _, change in ipairs{
  function(f) f.view.active_window_id = inactive end,
  function(f) f.rex.args.window = inactive end,
  function(f) f.rex.args.main = other end,
  function(f) f.rex.args.main = preview end,
  function(f) f.window.zoomed_block_id = main end,
  function(f) f.tiled.state = "minimized" end,
  function(f) f.window.layers[2] = { kind = "floating", blocks = {} } end,
  float_anchor,
  function(f)
    f.tiled.blocks = { f.rail, f.anchor }
    f.window.layers[2] = { kind = "floating", blocks = { f.main } }
  end,
  function(f) f.branch.after = nil end,
  function(f) f.leaf.rect = { 0, 0, 0, 1 } end,
  function(f) f.anchor.label = "milo:preview:starting" end,
  function(f) f.anchor.host_id = "remote" end,
  function(f) f.main.host_id = "remote" end,
  function(f) f.main.owner = other_app end,
  function(f) f.anchor.owner = other_app end,
  function(f) f.gui.principal = peer(gui + 1) end,
  function(f) f.gui.session_ids = { another_session } end,
  function(f) f.parents[cli][1] = helper + 1 end,
  function(f) f.proc[preview].child.pid = helper + 1 end,
  function(f) f.view.revision = nil end,
  function(f)
    local reads = 0
    f.rex.session.view = function()
      reads = reads + 1
      if reads == 1 then return f.view end
      return setmetatable({ revision = f.view.revision + 1 }, { __index = f.view })
    end
  end,
} do
  local stale = fixture("focus", preview)
  -- A denial must not acknowledge even if the working shell already has focus.
  change(stale)
  assert(call(stale)() == "focus\n" and not stale.focused)
end

local moved = fixture("watch", preview)
local watch_moved = call(moved)
assert(watch_moved() == shown)
move_preview_beside_rail(moved)
assert(watch_moved("session_view_changed") == shown .. stamp("show 40 tile 125000 0 125000 1000000") .. "\n")
assert(not moved.stopped)

local f = fixture("watch", preview)
local observe = call(f)
local expected = shown
assert(observe() == expected)
local function view_change(state)
  if state then expected = expected .. state .. "\n" end
  assert(observe("session_view_changed") == expected)
end
view_change() -- repeated snapshots do not repeatedly emit a state
f.main_leaf.rect[3], f.leaf.rect = 0.45, { 0.7, 0, 0.3, 1 }
view_change(stamp("show 40 tile 700000 0 300000 1000000"))
view_change()
-- The visible block list alone is not geometry. The exact tiled leaf must exist.
f.branch.after = nil
view_change("hide")
f.branch.after = f.leaf
for _, invalid in ipairs{ false, {}, { "0.7", 0, 0.3, 1 }, { 0.7, 0, 0.31, 1 },
  { 0.7, 0, 0.3, -1 }, { 0.7, 0/0, 0.3, 1 }, { 0.7, 0, 0.0000001, 1 } } do
  f.leaf.rect = invalid
  view_change()
  assert(not f.stopped)
end
f.leaf.rect = { 0.0078125, 0.125, 0.9921875, 0.875 }
view_change(stamp("show 40 tile 7813 125000 992187 875000"))
f.main_leaf.rect[3], f.leaf.rect = 0.375, { 0.625, 0, 0.375, 1 }
view_change(show)
f.view.active_window_id = inactive
assert(observe("session_view_changed", nil, { session_id = id("session", 99) }) == expected)
view_change("hide")
f.view.active_window_id = window
view_change(show)
f.window.zoomed_block_id = main
view_change("hide")
f.window.zoomed_block_id = preview
view_change()
f.window.zoomed_block_id = nil
view_change(show)
local float = { kind = "floating", blocks = {} }
f.window.layers[2] = float
view_change("hide")
float.state = "minimized"
view_change(show)
float.state = nil
view_change("hide")
f.window.layers[2] = nil
view_change(show)
f.tiled.state = "minimized"
view_change("hide")
f.tiled.state = nil
view_change(show)
f.tiled.blocks = { f.rail, f.main }
f.detached = true
view_change("hide")
assert(not f.stopped)
f.tiled.blocks, f.detached = { f.rail, f.main, f.anchor }, false
view_change(show)
f.gui.principal = peer(gui + 1)
view_change("hide") -- even the same client ID cannot substitute another GUI process
f.gui.client_id, f.anchor.owner = other_app, other_app
view_change()
f.gui.client_id, f.anchor.owner, f.gui.principal = app, app, peer(gui)
view_change(show)
assert(observe("block_closed", { block_id = preview }) == expected .. "hide\n" and f.stopped == 0)

for _, main_detached in ipairs{ false, true } do
  local moved = fixture("watch", preview)
  local watch_move = call(moved)
  float_anchor(moved)
  if main_detached then moved.tiled.blocks, moved.main_detached = { moved.rail }, true end
  assert(watch_move("session_view_changed") == shown .. "hide\n" and moved.stopped == 0)
end

-- The working block was reattached between the server's view and list responses.
local returning = fixture("watch", preview)
local watch_return = call(returning)
returning.tiled.blocks = { returning.rail, returning.anchor }
returning.rex.session.list_blocks = function() return { blocks = { { block_id = main, placement = "placed" } } } end
assert(watch_return("session_view_changed") == shown .. "hide\n" and not returning.stopped)
returning.tiled.blocks = { returning.rail, returning.main, returning.anchor }
assert(watch_return("session_view_changed") == shown .. shown)

local remote = fixture("watch", preview)
remote.gui.principal.kind = "tailscale"
assert(call(remote)() == "hide\n" and not remote.stopped)
local disconnected = fixture("watch", preview)
local lost = call(disconnected)
assert(lost("client_connection_changed", { client_id = app, connection_state = "lost" }) == shown .. "hide\n"
  and disconnected.stopped == 0)
local absent = fixture("watch", preview)
local unavailable = call(absent)
absent.view = nil
assert(unavailable("active_window_changed") == shown .. "hide\n" and absent.stopped == 1)
local invalid = fixture("watch", preview)
invalid.cli.pid = cli + 1
assert(call(invalid)() == "hide\n" and invalid.stopped == 1)
local hidden = fixture("watch", preview)
hidden.view.active_window_id = inactive
local first = call(hidden)
assert(first() == "hide\n")
hidden.gui.client_id, hidden.anchor.owner, hidden.gui.principal = other_app, other_app, peer(gui + 1)
assert(first("block_owner_changed") == "hide\n")
hidden.view.active_window_id = window
assert(first("active_window_changed") == "hide\n")
hidden.gui.client_id, hidden.anchor.owner, hidden.gui.principal = app, app, peer(gui)
assert(first("block_owner_changed") == shown)

local revisions = fixture("watch", preview)
local reads = 0
local view = revisions.rex.session.view
revisions.rex.session.view = function(args)
  reads = reads + 1
  return view(args)
end
local events = call(revisions)
assert(events() == shown and reads == 1)
assert(events("session_view_changed", { revision = 1 }) == shown and reads == 1)
revisions.view.revision, revisions.view.active_window_id = 3, inactive
assert(events("session_view_changed", { revision = 2 }) == shown .. "hide\n" and reads == 2)
assert(events("session_view_changed", { revision = 3 }) == shown .. "hide\n" and reads == 2)
revisions.view.active_window_id = window
assert(events("active_window_changed") == shown .. shown and reads == 3)
events("block_owner_changed")
events("client_connection_changed")
assert(reads == 5)

-- Scope to the exact app but never infer its selected session from its other
-- attachments. The pane stays eligible alongside the focused shell.
local multi = fixture("watch", preview)
multi.gui.session_ids = four_sessions
multi.second_viewer = second_viewer(gui + 1, { session })
local poll = call(multi)
assert(poll() == shown)
local persistent = shown
for tick = 1, 35 do
  if tick == 1 then multi.window.focused_block_id = preview end
  if tick == 2 then
    multi.window.focused_block_id = main
    multi.gui.session_ids = { session }
  end
  if tick == 3 then
    multi.gui.session_ids = four_sessions
    multi.second_viewer = second_viewer(gui, { another_session })
  end
  persistent = persistent .. (tick % 5 == 0 and show or stamp("renew")) .. "\n"
  assert(poll("heartbeat") == persistent)
end
assert(poll("deadline") == persistent and poll("claim_deadline") == persistent and not multi.stopped)
multi.gui.session_ids = { another_session }
assert(poll("heartbeat") == persistent .. "hide\n")
multi.gui.session_ids = four_sessions
persistent = persistent .. shown
assert(poll("heartbeat") == persistent)
multi.gui.connection_state = "lost"
assert(poll("heartbeat") == persistent .. "hide\n")
multi.gui.connection_state = "connected"
assert(poll("heartbeat") == persistent .. shown)

local ready = fixture("watch", preview)
ready.gui.session_ids = four_sessions
local watch_ready = call(ready)
assert(watch_ready() == shown)
-- Both tab changes committed before the first view() response. No layout
-- callback or heartbeat may revive the pane between the two events.
ready.view.revision = 3
local departed = shown .. "hide\n"
assert(watch_ready("active_window_changed", { window_id = inactive }) == departed)
assert(watch_ready("session_view_changed", { revision = 3 }) == departed)
assert(watch_ready("heartbeat") == departed)
local returned = shown .. shown
assert(watch_ready("active_window_changed", { window_id = window }) == returned)
assert(watch_ready("block_owner_changed", { block_id = other }) == returned)
-- A handoff observed after ownership has returned still revokes the old
-- state. Either terminal's ownership is required before showing again.
for _, pane in ipairs{ ready.anchor, ready.main } do
  assert(watch_ready("block_owner_changed", { block_id = pane.block_id }) == returned .. shown)
  returned = returned .. shown
  pane.owner = other_app
  assert(watch_ready("block_owner_changed", { block_id = pane.block_id }) == returned .. "hide\n")
  assert(watch_ready("heartbeat") == returned .. "hide\n")
  pane.owner = app
  returned = returned .. shown
  assert(watch_ready("block_owner_changed", { block_id = pane.block_id }) == returned)
end

-- Even without a queued event, renew only after a fresh check of geometry,
-- the original tab, and the two local panes.
local delayed = fixture("watch", preview)
local watch_delayed = call(delayed)
assert(watch_delayed() == shown)
delayed.view.revision = 4
delayed.leaf.rect = { 0.7, 0, 0.3, 1 }
local resized_show = stamp("show 40 tile 700000 0 300000 1000000") .. "\n"
local resized = shown .. resized_show
assert(watch_delayed("heartbeat") == resized)
assert(watch_delayed("session_view_changed", { revision = 4 }) == resized)
delayed.view.active_window_id = inactive
assert(watch_delayed("heartbeat") == resized .. "hide\n")
delayed.view.active_window_id = window
assert(watch_delayed("heartbeat") == resized .. "hide\n" .. resized_show)

-- Both positive records carry the time before any RPC, not the time
-- after the server responds. Simulate an RPC delayed through the lease.
local waiting = fixture("watch", preview)
local rpc = waiting.rex.call
waiting.now = epoch
waiting.rex.call = function(method, args)
  if method == "client.list" then waiting.now = waiting.now + 9 end
  return rpc(method, args)
end
local watch_waiting = call(waiting)
assert(waiting.now == epoch + 9 and watch_waiting() == shown)
assert(watch_waiting("heartbeat") == shown .. stamp("renew", epoch + 9) .. "\n" and waiting.now == epoch + 18)

-- A backwards wall clock can invalidate the helper's announced geometry.
-- Once wall time recovers, a later full snapshot repairs the baseline
-- without any tab, focus or geometry change.
local clock = fixture("watch", preview)
local watch_clock = call(clock)
assert(watch_clock() == shown)
local clock_log = shown
local tile = "show 40 tile 625000 0 375000 1000000"
for tick = 1, 10 do
  clock.now = (tick <= 5 and epoch - 60 or epoch) + tick
  clock_log = clock_log .. stamp(tick % 5 == 0 and tile or "renew", clock.now) .. "\n"
  assert(watch_clock("heartbeat") == clock_log)
end
assert(not clock.stopped and clock.window.focused_block_id == main)

for _, side in ipairs{ "main", "anchor" } do
  local remote_pane = fixture("watch", preview)
  local watch = call(remote_pane)
  assert(watch() == shown)
  remote_pane[side].host_id = "remote"
  assert(watch("heartbeat") == shown .. "hide\n" and remote_pane.stopped == 1)
end

local staged = fixture("watch", preview)
staged.anchor.label = "milo:preview:starting"
staged.anchor.owner = nil
staged.rex.args.startup_fd = 7
local gate = call(staged)
assert(gate() == "hide\n" and staged.ready)
staged.anchor.label = "milo:preview"
assert(gate("session_view_changed") == "hide\n")
staged.anchor.owner = app
assert(gate("block_owner_changed") == shown)
assert(gate("deadline") == shown and not staged.stopped)
local refused_startup = fixture("watch", preview)
refused_startup.rex.args.startup_fd = 7
refused_startup.proc[preview].child.pid = helper + 1
assert(call(refused_startup)() == "hide\n" and refused_startup.stopped == 1 and not refused_startup.ready)
local unpublished = fixture("watch", preview)
unpublished.anchor.label = "milo:preview:starting"
assert(call(unpublished)("deadline") == "hide\n" and unpublished.stopped == 0)
local staged_detached = fixture("watch", preview)
staged_detached.anchor.label = "milo:preview:starting"
staged_detached.detached, staged_detached.tiled.blocks = true, { staged_detached.rail, staged_detached.main }
assert(call(staged_detached)("deadline") == "hide\n" and staged_detached.stopped == 0)

local unclaimed = fixture("watch", preview)
unclaimed.anchor.owner = nil
local watch_unclaimed = call(unclaimed)
assert(watch_unclaimed("deadline") == "hide\n" and not unclaimed.stopped)
assert(watch_unclaimed("heartbeat") == "hide\n" and not unclaimed.stopped)
assert(watch_unclaimed("claim_deadline") == "hide\nunclaimed\n" and unclaimed.stopped == 1)

local different_owner = fixture("watch", preview)
different_owner.anchor.owner = other_app
local watch_foreign = call(different_owner)
assert(watch_foreign("claim_deadline") == "hide\nunclaimed\n" and different_owner.stopped == 1)

local claimed_hidden = fixture("watch", preview)
claimed_hidden.view.active_window_id = inactive
local watch_claimed = call(claimed_hidden)
assert(watch_claimed() == "hide\n")
claimed_hidden.anchor.owner = nil
assert(watch_claimed("block_owner_changed") == "hide\n")
assert(watch_claimed("claim_deadline") == "hide\n" and not claimed_hidden.stopped)

local claim_race = fixture("watch", preview)
claim_race.anchor.owner = nil
claim_race.rex.session.describe_block = function()
  claim_race.anchor.owner = app
  return { block = claim_race.anchor }
end
local watch_claim_race = call(claim_race)
assert(watch_claim_race("claim_deadline") == "hide\n" and not claim_race.stopped)
assert(watch_claim_race("block_owner_changed") == shown)

for _, side in ipairs{ "main", "anchor" } do
  local initial = fixture("watch", preview)
  if side == "main" then
    initial.main_detached, initial.tiled.blocks = true, { initial.rail, initial.anchor }
  else
    initial.detached, initial.tiled.blocks = true, { initial.rail, initial.main }
    -- Miss the publication event while detached; the timer itself must
    -- recheck the authoritative label rather than using the stale latch.
    initial.anchor.label = "milo:preview:starting"
  end
  local watch = call(initial)
  assert(watch() == "hide\n" and not initial.stopped)
  initial.anchor.label = "milo:preview"
  assert(watch("deadline") == "hide\n" and not initial.stopped)
  initial.detached, initial.main_detached = false, false
  initial.tiled.blocks = { initial.rail, initial.main, initial.anchor }
  assert(watch("session_view_changed") == shown and not initial.stopped)
end

local wrong_detached = fixture("watch", preview)
wrong_detached.detached, wrong_detached.tiled.blocks = true, { wrong_detached.rail, wrong_detached.main }
wrong_detached.proc[preview].child.pid = helper + 1
assert(call(wrong_detached)() == "hide\n" and wrong_detached.stopped == 1)
local forged = fixture("watch", preview)
forged.parents[cli][1] = 99
assert(call(forged)() == "hide\n" and forged.stopped == 1)
local mislabeled = fixture("watch", preview)
mislabeled.anchor.label = "different label"
assert(call(mislabeled)() == "hide\n" and mislabeled.stopped == 1)
local detached_main = fixture("watch", preview)
local pairing = call(detached_main)
detached_main.tiled.blocks, detached_main.main_detached = { detached_main.rail, detached_main.anchor }, true
assert(pairing("session_view_changed") == shown .. "hide\n" and not detached_main.stopped)
detached_main.tiled.blocks, detached_main.main_detached = { detached_main.rail, detached_main.main, detached_main.anchor }, false
assert(pairing("session_view_changed") == shown .. shown)
assert(pairing("block_closed", { block_id = main }) == shown .. shown .. "hide\n"
  and detached_main.stopped == 0)
for _, detach_main in ipairs{ false, true } do
  local moved = fixture("watch", preview)
  local watch_move = call(moved)
  moved.main_detached = detach_main
  moved.tiled.blocks = detach_main and { moved.rail } or { moved.rail, moved.main }
  moved.inactive.layers[1].blocks = { moved.other, moved.anchor }
  moved.view.active_window_id = inactive
  assert(watch_move("active_window_changed", { window_id = inactive }) == shown .. "hide\n" and moved.stopped == 0)
end
