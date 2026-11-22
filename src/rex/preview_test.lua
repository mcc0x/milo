-- With Lua 5.1: lua5.1 src/rex/preview_test.lua src/rex/preview.lua
local script = assert(arg[1])
local function id(kind, n)
  return string.format("%s:00000000-0000-0000-0000-%012x", kind, n)
end
local session, window, inactive = id("session", 1), id("window", 1), id("window", 2)
local main, sidebar, preview, other = id("block", 1), id("block", 2), id("block", 3), id("block", 4)
local app, other_app = id("client", 1), id("client", 2)
local host = "00000000-0000-0000-0000-000000000001"
local term = "com.superlogical.terminal"
local exe, url = "/with spaces/milo", "http://localhost:3400/hello?who=$HOME&next=%2Fhere#section"
local uid, helper, cli, gui = 501, 20, 30, 40
local server = "unix:///tmp/rex%20preview.sock"
local function peer(pid) return { kind = "peer", transport = "unix", uid = uid, pid = pid } end
local function terminal(block, label)
  return { block_id = block, label = label, creator_name = term, host_id = host, owner = app }
end
local function call(f)
  local args = f.rex.args
  local writes, callbacks = {}, {}
  local env = setmetatable({
    rex = f.rex,
    print = function(s) writes[#writes + 1] = s .. "\n" end,
    io = { stdout = { write = function(_, s, n) writes[#writes + 1] = s .. n end, flush = function() end } },
  }, { __index = _G })
  f.rex.on = function(event, run)
    assert(not callbacks[event], event)
    callbacks[event] = run
  end
  local after
  f.rex.after = function(delay, fn) assert(delay == 0); after = fn end
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
      connection_state = "connected", principal = peer(gui) },
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
  f.window = { window_id = window, layers = { f.tiled }, focused_block_id = main }
  f.inactive = { window_id = inactive, layers = { { kind = "tiled", blocks = { f.other } } } }
  f.view = { session_id = session, active_window_id = window, revision = 1, windows = { f.window, f.inactive } }
  local function scope(a) assert(a.session_id == session) end
  local function placed(api, a)
    scope(a)
    assert(not f.placed)
    f.placed = { api = api, args = a }
    return { block_ids = { preview } }
  end
  f.rex = {
    session_id = session,
    block_id = block,
    args = { mode = mode, server = server, uid = uid, pid = helper, session = session, block = block,
      exe = exe, url = url, placement = "right", main = main, cwd = "/work/project", owner = app, owner_pid = gui },
    call = function(method, a)
      if method == "server.whoami" then return { principal = f.cli } end
      if method == "server.status" then return f.status end
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
      list_clients = function(a) scope(a); return { clients = { f.gui } } end,
      list_blocks = function(a) scope(a); return { blocks = f.detached and { f.anchor } or {} } end,
      new_split = function(a) return placed("split", a) end,
      new_layer = function(a) return placed("floating", a) end,
      focus_block = function(a) return placed("focus", a) end,
    },
  }
  return f
end

for _, p in ipairs{ { "right", "horizontal", "after" }, { "left", "horizontal", "before" },
  { "above", "vertical", "before" }, { "below", "vertical", "after" }, { "floating" } } do
  local f = fixture("open", sidebar)
  f.rex.args.placement = p[1]
  assert(call(f)() == "ok\n")
  local a = f.placed.args
  assert(a.focus == false and a.layout.block.label == "milo:preview")
  local launch = a.layout.block.options
  assert(launch.shell == "none" and launch.cwd == "/work/project")
  assert(#launch.command == 6 and launch.command[1] == exe and launch.command[2] == "preview-anchor"
    and launch.command[3] == url and launch.command[4] == main
    and launch.command[5] == app and launch.command[6] == tostring(gui))
  if p[1] == "floating" then
    assert(f.placed.api == "floating" and a.window_id == window and #a.bounds == 4 and not a.anchor_id)
  else
    assert(f.placed.api == "split" and a.anchor_id == main and a.direction == p[2] and a.side == p[3])
  end
end

local function denied(mode, block, change, status)
  local f = fixture(mode, block)
  change(f)
  assert(call(f)() == status .. "\n" and not f.placed)
end
denied("open", sidebar, function(f) f.referenced = other end, "stale")
denied("open", sidebar, function(f) f.referenced = "block:0" end, "stale")
denied("open", sidebar, function(f) f.view.active_window_id = inactive end, "tab")
denied("open", sidebar, function(f) f.main.host_id = host .. "remote" end, "remote")
denied("open", main, function(f) f.proc[main].foreground = { name = "ssh" } end, "remote")
denied("open", main, function(f)
  f.proc[main].child.argv0, f.shell_command = "zsh", "zsh -ic 'echo text'"
end, "shell")
local interactive = fixture("open", main)
interactive.proc[main].child.argv0, interactive.shell_command = "zsh", "zsh -i"
assert(call(interactive)() == "ok\n" and interactive.placed.args.anchor_id == main)
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
assert(call(wrapped)() == "ok\n")
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
assert(call(nested)() == "ok\n" and nested.placed.args.layout.block.options.cwd == inner.cwd)
denied("open", sidebar, function(f) f.rex.args.main = other end, "changed")
denied("open", sidebar, function(f)
  f.rail.label = "milo:usage"
  f.tiled.blocks[#f.tiled.blocks + 1] = f.other
  f.inactive.layers[1].blocks = {}
  f.proc[other] = f.proc[main]
end, "shell")

local focused = fixture("focus", preview)
assert(call(focused)() == "ok\n" and focused.placed.api == "focus" and focused.placed.args.block_id == main)
denied("focus", preview, function(f) f.rex.args.main = other end, "focus")
denied("focus", preview, function(f) f.view.active_window_id = inactive end, "tab")
denied("focus", preview, function(f) f.window.zoomed_block_id = preview end, "focus")
denied("focus", preview, function(f) f.proc[preview].child.pid = helper + 1 end, "focus")
denied("focus", preview, function(f) f.rex.args.owner = other_app end, "focus")
denied("focus", preview, function(f) f.rex.args.owner_pid = gui + 1 end, "focus")

local f = fixture("watch", preview)
local observe = call(f)
local expected = "hide\nshow 40 tile\n"
assert(observe() == expected)
local function view_change(state)
  if state then expected = expected .. state .. "\n" end
  assert(observe("session_view_changed") == expected)
end
view_change() -- repeated snapshots do not repeatedly emit a state
f.view.active_window_id = inactive
assert(observe("session_view_changed", nil, { session_id = id("session", 99) }) == expected)
view_change("hide")
f.view.active_window_id = window
view_change("show 40 tile")
f.window.zoomed_block_id = main
view_change("hide")
f.window.zoomed_block_id = preview
view_change("show 40 tile")
local float = { kind = "floating", blocks = {} }
f.window.layers[2] = float
view_change("hide")
float.state = "minimized"
view_change("show 40 tile")
float.state = nil
view_change("hide")
f.window.zoomed_block_id = nil
view_change()
f.tiled.blocks = { f.rail, f.main }
float.blocks = { f.anchor }
view_change("show 40 float")
float.state = "minimized"
view_change("hide")
float.state = nil
view_change("show 40 float")
f.window.layers[3] = { kind = "floating", blocks = {} }
view_change("hide")
f.window.layers[3].state = "minimized"
view_change("show 40 float")
float.blocks, f.detached = {}, true
view_change("hide")
assert(not f.stopped)
float.blocks, f.detached = { f.anchor }, false
view_change("show 40 float")
f.gui.principal = peer(gui + 1)
view_change("hide") -- even the same client ID cannot substitute another GUI process
f.gui.client_id, f.anchor.owner = other_app, other_app
view_change()
f.gui.client_id, f.anchor.owner, f.gui.principal = app, app, peer(gui)
view_change("show 40 float")
assert(observe("block_closed", { block_id = preview }) == expected .. "hide\n" and f.stopped == 0)

local remote = fixture("watch", preview)
remote.gui.principal.kind = "tailscale"
assert(call(remote)() == "hide\n" and not remote.stopped)
local disconnected = fixture("watch", preview)
local lost = call(disconnected)
assert(lost("client_connection_changed", { client_id = app, connection_state = "lost" }) == "hide\nshow 40 tile\nhide\n"
  and disconnected.stopped == 0)
local absent = fixture("watch", preview)
local unavailable = call(absent)
absent.view = nil
assert(unavailable("active_window_changed") == "hide\nshow 40 tile\nhide\n" and absent.stopped == 1)
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
assert(first("block_owner_changed") == "hide\nshow 40 tile\n")

local revisions = fixture("watch", preview)
local reads = 0
local view = revisions.rex.session.view
revisions.rex.session.view = function(args)
  reads = reads + 1
  return view(args)
end
local events = call(revisions)
assert(events() == "hide\nshow 40 tile\n" and reads == 1)
assert(events("session_view_changed", { revision = 1 }) == "hide\nshow 40 tile\n" and reads == 1)
revisions.view.revision, revisions.view.active_window_id = 3, inactive
assert(events("session_view_changed", { revision = 2 }) == "hide\nshow 40 tile\nhide\n" and reads == 2)
assert(events("session_view_changed", { revision = 3 }) == "hide\nshow 40 tile\nhide\n" and reads == 2)
revisions.view.active_window_id = window
assert(events("active_window_changed") == "hide\nshow 40 tile\nhide\nshow 40 tile\n" and reads == 3)
events("block_owner_changed")
events("client_connection_changed")
assert(reads == 5)
