-- Run with Lua 5.1 against listener.lua from a saved layout with new shell tabs on.
local handlers, timers, splits, windows, processes = {}, {}, {}, {}, {}
local saves, revision = {}, 1
local measured = { rows = 50, owner = "app", desired = {{ client_id = "app", rows = 50 }} }
local sizes = {}
local function terminal(method, args)
  assert(args.session_id == "project")
  if method == "size" then return sizes[args.block_id] or measured end
  assert(method == "process")
  return processes[args.block_id]
end
local remembered, on_profile, executable, attached
rex = {
  on = function(event, fn) handlers[event] = fn end,
  sleep = function() error("a starting shell held up the listener") end,
  after = function(wait, fn)
    assert(wait == 0 or wait == 0.2, "new tabs must not start retry timers")
    local cancelled
    timers[#timers + 1] = function() if not cancelled then fn() end end
    return { cancel = function() cancelled = true end }
  end,
  session = {
    view = function(args)
      assert(attached, "cannot read a baseline before attach_all")
      assert(args.session_id == "project")
      for _, window in ipairs(windows) do
        for _, block in ipairs(window.layers[1].blocks) do
          block.owner = (sizes[block.block_id] or measured).owner
        end
      end
      return { windows = windows, revision = revision }
    end,
    new_split = function(args) table.insert(splits, args); return {} end,
  },
  call = function(method, args)
    if method == "session.list" then
      assert(attached, "cannot read a baseline before attach_all")
      return { sessions = {{ session_id = "project" }} }
    end
    return terminal(method:match("%.([^.]*)$"), args)
  end,
  exec = function(argv)
    executable = argv[1]
    if argv[3] == "resize" then
      assert(argv[2] == "layout-profile" and argv[6] == "/my project" and #argv == 6)
      saves[#saves + 1], remembered = argv, argv[5]
      return { ok = true }
    end
    assert(argv[2] == "layout-profile" and argv[3] == "read" and argv[4] == "/my project")
    if on_profile then on_profile() end
    return { ok = true, stdout = remembered or "" }
  end,
  block = { call = function(_, method, args) args.session_id = "project"; return terminal(method, args) end },
  layout = {
    block = function(args) return args end,
    vertical = function(ratio, a, b) return { a, b, ratio = ratio } end,
  },
}
local function boot(listener, before_baseline)
  attached = false
  local first = #timers + 1
  assert(listener)()
  attached = true
  if before_baseline then before_baseline() end
  table.remove(timers, first)()
end
-- Setup must be able to validate the generated listener outside a live session.
local scheduler = rex.after
rex.after = nil
assert(loadfile(arg[1]))()
rex.after = scheduler
boot(loadfile(assert(arg[1])))
local function tab(id, ...)
  local blocks = {}
  for _, block in ipairs({...}) do blocks[#blocks + 1] = { block_id = block, creator_name = "com.superlogical.terminal" } end
  local layout = #blocks == 1 and { block_id = blocks[1].block_id, rect = {0, 0, 1, 1} } or nil
  return { window_id = id, layers = {{ kind = "tiled", layer_id = "layer:" .. id, layout = layout, blocks = blocks }} }
end
local function created(id) handlers.window_created({ session_id = "project" }, { window_id = id }) end
local function emit(event, block)
  handlers[event]({ session_id = "project", block_id = block }, { block_id = block })
end
local function changed()
  revision = revision + 1
  handlers.session_view_changed({ session_id = "project" }, { revision = revision })
end
local project = { foreground = { argv0 = "-zsh", cwd = "/my project" } }

-- Waiting for a slow shell or a client measurement does not delay another tab.
timers, splits = {}, {}
windows = { tab("slow", "slow"), tab("quick", "quick"), tab("command", "command"), tab("unowned", "unowned") }
processes = {
  slow = { child = { path = "/usr/bin/login" }, foreground = { argv0 = "login" } },
  quick = project,
  command = { foreground = { argv0 = "build" } },
  unowned = project,
}
sizes.unowned = { rows = 24 }
created("slow")
created("unowned")
assert(#timers == 0 and #splits == 0)
created("command")
assert(#splits == 0)
created("quick")
assert(#splits == 1 and splits[1].anchor_id == "quick" and splits[1].side == "before")
-- The usage takes its rows from the foot of the column, below the outline and history.
local column = splits[1].layout
assert(column[1][1].options.cwd == "/my project" and column[1][2].label == "milo:history")
assert(column[2].label == "milo:usage" and math.abs(column.ratio - 0.96) < 0.000001 and splits[1].ratio == 0.20)
assert(not splits[1].focus)
assert(column[1][1].options.command[3] == "--shell" and column[1][1].options.command[4] == "quick")
assert(column[2].options.command[3] == nil)
emit("terminal.size_changed", "slow")
assert(#splits == 1)
processes.slow = project
emit("terminal.process_changed", "slow")
assert(#splits == 2 and splits[2].anchor_id == "slow")

-- Owning a tab isn't the same as measuring it. Only the owner's measurement counts.
sizes.unowned = { rows = 24, owner = "app", desired = {{ client_id = "other", rows = 24 }} }
emit("block_owner_changed", "unowned")
assert(#splits == 2)
sizes.unowned = measured
emit("terminal.size_changed", "unowned")
assert(#splits == 3 and splits[3].anchor_id == "unowned")
assert(math.abs(splits[3].layout.ratio - 0.96) < .000001)
emit("terminal.size_changed", "unowned")
processes.command = project
emit("terminal.process_changed", "command")
assert(#splits == 3 and #timers == 0)

-- A new session's first tab is arranged too, though no tab is announced for it.
splits = {}
windows = { tab("first", "first") }
processes = { first = project }
handlers.session_created({}, { session = { session_id = "project" } })
assert(#splits == 1 and splits[1].anchor_id == "first")
windows = { tab("slow", "slow"), tab("quick", "quick"), tab("command", "command") }

-- A remembered profile takes precedence on a new shell tab, including right
-- placement, top-to-bottom order and shares rather than legacy Usage rows.
splits = {}
sizes.quick = { rows = 24 }
remembered = "v1 r 0.28 0.70 10 uho 011 0 300000 700000"
processes.quick = project
created("quick")
assert(#splits == 1 and splits[1].side == "after" and math.abs(splits[1].ratio - 0.72) < 0.000001)
assert(splits[1].layout[1].label == "milo:usage" and splits[1].layout[2].label == "milo:history")
assert(math.abs(splits[1].layout.ratio - 0.7) < 0.000001)
sizes.quick = measured

-- A split created during profile lookup wins even if the tab was pristine when the event arrived.
splits = {}
on_profile = function() windows[2] = tab("quick", "quick", "user split") end
created("quick")
assert(#splits == 0)
on_profile, remembered = nil, nil

on_profile = function() processes.quick = { foreground = { argv0 = "build" } } end
windows[2] = tab("quick", "quick")
created("quick")
on_profile, processes.quick = nil, project
emit("terminal.process_changed", "quick")
assert(#splits == 0)

-- A cancelled lookup may not split even if the tab is already pristine again.
on_profile = function()
  windows[2] = tab("quick", "quick", "user split")
  changed()
  windows[2] = tab("quick", "quick")
end
created("quick")
assert(#splits == 0)

on_profile = function()
  processes.quick = { foreground = { argv0 = "build" } }
  emit("terminal.process_changed", "quick")
  processes.quick = project
end
created("quick")
assert(#splits == 0)

-- Repeated readiness signals cannot commit the same arrangement twice.
on_profile = function()
  on_profile = nil
  emit("terminal.size_changed", "quick")
end
created("quick")
assert(#splits == 1 and splits[1].anchor_id == "quick")

-- A shell split by the user while waiting stays cancelled even after an undo.
splits, processes = {}, {}
windows[1] = tab("slow", "slow")
created("slow")
windows[1] = tab("slow", "slow", "user split")
changed()
windows[1] = tab("slow", "slow")
processes.slow = project
emit("terminal.process_changed", "slow")
assert(#timers == 0 and #splits == 0)

-- A command started while waiting for measurement wins, even once it exits.
sizes.slow = { rows = 24 }
created("slow")
processes.slow = { foreground = { argv0 = "build" }, child = { path = "/usr/bin/login" } }
emit("terminal.process_changed", "slow")
processes.slow, sizes.slow = project, measured
emit("terminal.size_changed", "slow")
assert(#splits == 0)

-- A terminal starts as a copy of the Rex server. A measurement that arrives
-- while that copy is in the foreground has met no command: the tab is arranged
-- once its shell appears, and left alone if the copy becomes a command instead.
local starting = {
  child = { pid = 7, name = "rex", path = "/opt/rex" },
  foreground = { pid = 7, name = "rex", argv0 = "/opt/rex", path = "/opt/rex" },
}
processes.slow = starting
created("slow")
emit("terminal.size_changed", "slow")
assert(#splits == 0)
processes.slow = project
emit("terminal.process_changed", "slow")
assert(#splits == 1 and splits[1].anchor_id == "slow")
splits, processes.slow = {}, starting
created("slow")
processes.slow = { child = { pid = 7, name = "build", path = "/bin/build" }, foreground = { pid = 7, argv0 = "build" } }
emit("terminal.process_changed", "slow")
processes.slow = project
emit("terminal.process_changed", "slow")
assert(#splits == 0)

-- A shell copies itself as it starts and as it draws its prompt. A copy in the
-- foreground when the measurement arrives is the same shell. A login shell
-- that another program started in the tab is not.
local login = { pid = 1, path = "/usr/bin/login" }
local own = { pid = 2, argv0 = "-zsh", path = "/bin/zsh", cwd = "/my project" }
local function copy(pid) return { pid = pid, argv0 = "-zsh", path = "/bin/zsh", cwd = "/my project" } end
for _, other in ipairs({ false, true }) do
  splits, sizes.slow = {}, { rows = 24 }
  processes.slow = { child = login, foreground = own, ancestors = { login } }
  created("slow")
  local between = other and { { pid = 3, argv0 = "su", path = "/usr/bin/su" }, own, login } or { own, login }
  processes.slow, sizes.slow = { child = login, foreground = copy(4), ancestors = between }, measured
  emit("terminal.size_changed", "slow")
  assert(#splits == (other and 0 or 1))
end

-- Close and exit events remove outstanding work, including keep-open terminals.
for _, event in ipairs({ "block_closed", "terminal.child_exited", "window_closed", "session_destroyed" }) do
  processes.slow = nil
  created("slow")
  handlers[event]({ session_id = "project" }, { session_id = "project", window_id = "slow", block_id = "slow" })
  processes.slow = project
  emit("terminal.process_changed", "slow")
  assert(#splits == 0)
end

local function rail(id, width, height, exe)
  local window = tab(id, id .. "-shell", id .. "-outline", id .. "-usage")
  local layer = window.layers[1]
  local shell, outline, usage = layer.blocks[1], layer.blocks[2], layer.blocks[3]
  outline.label, usage.label = "milo:outline", "milo:usage"
  local function leaf(block) return { block_id = block.block_id } end
  local stack = { split = { split_id = id .. "-height", direction = "vertical",
    ratio = height, before = leaf(outline), after = leaf(usage) } }
  layer.layout = { split = { split_id = id .. "-width", direction = "horizontal",
    ratio = width, before = stack, after = leaf(shell) } }
  local pane = { child = { path = exe, cwd = "/my project" } }
  processes[shell.block_id] = { foreground = { argv0 = "-zsh", cwd = "/different worktree" } }
  processes[outline.block_id], processes[usage.block_id] = pane, pane
  return window
end

-- An open rail can be resized on the first event after attach. If an event
-- already seeded a baseline, the startup timer must not replace it.
for _, event_first in ipairs({false, true}) do
  processes = {}
  windows = { rail("startup", .2, .8, executable) }
  local split = windows[1].layers[1].layout.split
  boot(loadfile(arg[1]), function()
    if event_first then
      changed()
      split.ratio = .3
    end
  end)
  split.ratio = .3
  changed()
  table.remove(timers, 1)()
  assert(#saves == 1 and saves[1][4]:match("^v1 l 0%.200000 ") and saves[1][5]:match("^v1 l 0%.300000 "))
  saves, remembered = {}, nil
end

-- Startup, focus, other tabs' initial rails, and replacing or moving split trees
-- establish baselines but never save. A foreign process cannot pose as a Milo pane.
processes = {}
windows = { rail("live", .2, .8, executable), rail("foreign", .2, .8, "/bin/zsh") }
boot(loadfile(arg[1]))
changed()
windows[3] = rail("another", .3, .9, executable)
windows[4] = rail("trial", .2, .8, executable)
windows[4].layers[1].blocks[2].label = "milo:trial:outline"
windows[4].layers[1].blocks[3].label = "milo:trial:usage"
changed()
windows[3].layers[1].layout.split.split_id = "replacement"
windows[3].layers[1].layout.split.ratio = .4
windows[2].layers[1].layout.split.ratio = .35
windows[4].layers[1].layout.split.ratio = .35
changed()
assert(#timers == 0 and #saves == 0)

-- Coalesce repeated updates during the drag; closing immediately flushes
-- the last geometry even though the rail's processes are already gone.
windows[1].layers[1].layout.split.before.split.ratio = .7
changed()
windows[1].layers[1].layout.split.before.split.ratio = .6
changed()
table.remove(windows, 1)
processes["live-outline"], processes["live-usage"] = nil, nil
handlers.window_closed({}, { window_id = "live" })
for _, timer in ipairs(timers) do timer() end
assert(#saves == 1 and saves[1][4]:match("800000 0 200000$") and saves[1][5]:match("600000 0 400000$"))

-- Reopening the same project replays the geometry without another save.
splits, timers = {}, {}
windows[1] = tab("reopened", "reopened")
processes.reopened = project
created("reopened")
assert(#splits == 1 and math.abs(splits[1].layout.ratio - .6) < .000001 and #saves == 1)

-- With auto-arrange off, the listener keeps tracking manually arranged rails.
-- The optional second argument is the generated actions file after disabling it.
if arg[2] then
  local original_loadfile = loadfile
  loadfile = function() return original_loadfile(arg[2]) end
  boot(original_loadfile(arg[1]))
  local actions, launches, exec = {}, 0, rex.exec
  rex.action = function(action) actions[action.name] = action.run end
  rex.exec = setmetatable({ start = function() launches = launches + 1 end }, {
    __call = function(_, ...) return exec(...) end,
  })
  assert(original_loadfile(arg[2]))()
  assert(launches == 1, "the listener must still launch when auto-arrange is off")

  splits = {}
  windows[1] = tab("manual", "manual")
  processes.manual = project
  created("manual")
  assert(#splits == 0)
  actions.milo_layout({ session_id = "project", block_id = "manual" })
  assert(#splits == 1 and math.abs(splits[1].layout.ratio - .6) < .000001)
  windows[1] = rail("manual", .2, .6, executable)
  changed()
  windows[1].layers[1].layout.split.ratio = .36
  changed()
  for _, timer in ipairs(timers) do timer() end
  assert(#saves == 2 and saves[2][5]:match("^v1 l 0%.360000 "))
  loadfile = original_loadfile
end
