-- Run with Lua 5.1 against listener.lua from a saved layout with new shell tabs on.
local handler, timers, splits, windows, processes
local function terminal(method, args)
  assert(args.session_id == "project")
  if method == "size" then return { rows = 25 } end
  assert(method == "process")
  return processes[args.block_id]
end
rex = {
  on = function(event, fn) assert(event == "window_created"); handler = fn end,
  sleep = function() error("a starting shell held up the listener") end,
  after = function(wait, fn) assert(wait == 0.05); table.insert(timers, fn) end,
  session = {
    view = function(args) assert(args.session_id == "project"); return { windows = windows } end,
    new_split = function(args) table.insert(splits, args); return {} end,
  },
  call = function(method, args) return terminal(method:match("%.([^.]*)$"), args) end,
  block = { call = function(_, method, args) args.session_id = "project"; return terminal(method, args) end },
  layout = {
    block = function(args) return args end,
    vertical = function(_, a, b) return { a, b } end,
  },
}
assert(loadfile(assert(arg[1])))()
local function tab(id, ...)
  local blocks = {}
  for _, block in ipairs({...}) do blocks[#blocks + 1] = { block_id = block } end
  return { window_id = id, layers = {{ kind = "tiled", blocks = blocks }} }
end
local function created(id) handler({ session_id = "project" }, { window_id = id }) end
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
assert(#splits == 2 and splits[1].anchor_id == "quick" and splits[2].anchor_id == "quick")
assert(splits[1].layout[1].options.cwd == "/my project")
assert(splits[2].layout.label == "milo:usage" and splits[2].ratio == 0.2)
processes.slow = project
table.remove(timers, 1)()
assert(#splits == 4 and splits[3].anchor_id == "slow" and splits[4].anchor_id == "slow")

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
