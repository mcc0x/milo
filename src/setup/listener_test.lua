-- Run with Lua 5.1 against listener.lua from a saved layout with new shell tabs on.
local handlers, timers, splits, windows, processes = {}
local function terminal(method, args)
  assert(args.session_id == "project")
  if method == "size" then return { rows = 50, owner = "app" } end
  assert(method == "process")
  return processes[args.block_id]
end
local remembered, on_profile
rex = {
  on = function(event, fn) handlers[event] = fn end,
  sleep = function() error("a starting shell held up the listener") end,
  after = function(wait, fn) assert(wait == 0.05); table.insert(timers, fn) end,
  session = {
    view = function(args) assert(args.session_id == "project"); return { windows = windows } end,
    new_split = function(args) table.insert(splits, args); return {} end,
  },
  call = function(method, args) return terminal(method:match("%.([^.]*)$"), args) end,
  exec = function(argv)
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
assert(loadfile(assert(arg[1])))()
local function tab(id, ...)
  local blocks = {}
  for _, block in ipairs({...}) do blocks[#blocks + 1] = { block_id = block } end
  return { window_id = id, layers = {{ kind = "tiled", blocks = blocks }} }
end
local function created(id) handlers.window_created({ session_id = "project" }, { window_id = id }) end
local project = { foreground = { argv0 = "-zsh", cwd = "/my project" } }

-- One slow shell must not delay another tab's layout or turn command tabs into shell tabs.
timers, splits = {}, {}
windows = { tab("slow", "slow"), tab("quick", "quick"), tab("command", "command") }
processes = { quick = project, command = { foreground = { argv0 = "build" } } }
created("slow")
assert(#timers == 1 and #splits == 0)
created("command")
assert(#splits == 0)
created("quick")
assert(#splits == 1 and splits[1].anchor_id == "quick" and splits[1].side == "before")
-- The usage takes its rows from the foot of the column, below the outline and history.
local column = splits[1].layout
assert(column[1][1].options.cwd == "/my project" and column[1][2].label == "milo:history")
assert(column[2].label == "milo:usage" and math.abs(column.ratio - 0.8) < 0.000001 and splits[1].ratio == 0.20)
assert(not splits[1].focus)
assert(column[1][1].options.command[3] == "--shell" and column[1][1].options.command[4] == "quick")
assert(column[2].options.command[3] == nil)
processes.slow = project
table.remove(timers, 1)()
assert(#splits == 2 and splits[2].anchor_id == "slow")

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
remembered = "v1 r 0.28 0.70 10 uho 011 0 300000 700000"
processes.quick = project
created("quick")
assert(#splits == 1 and splits[1].side == "after" and math.abs(splits[1].ratio - 0.72) < 0.000001)
assert(splits[1].layout[1].label == "milo:usage" and splits[1].layout[2].label == "milo:history")
assert(math.abs(splits[1].layout.ratio - 0.7) < 0.000001)

-- A split created during profile lookup wins even if the tab was pristine when the event arrived.
splits = {}
on_profile = function() windows[2] = tab("quick", "quick", "user split") end
created("quick")
assert(#splits == 0)
on_profile, remembered = nil, nil

-- A shell split by the user while waiting must not get Milo panes.
timers, splits, processes = {}, {}, {}
windows[1] = tab("slow", "slow")
created("slow")
windows[1] = tab("slow", "slow", "user split")
processes.slow = project
table.remove(timers, 1)()
assert(#timers == 0 and #splits == 0)

-- A shell that never starts must not keep a timer alive forever.
timers, splits, processes = {}, {}, {}
windows[1] = tab("slow", "slow")
created("slow")
for _ = 1, 9 do table.remove(timers, 1)() end
assert(#timers == 0 and #splits == 0)

-- A note below the launcher must not keep a disabled listener running.
-- The optional second argument comes from turning off new tabs after adding such a note.
if arg[2] then
  local launches = 0
  rex.action = function() end
  rex.exec = { start = function() launches = launches + 1 end }
  assert(loadfile(arg[2]))()
  assert(launches == 0, "turning off new shell tabs must stop launching the listener")
end
