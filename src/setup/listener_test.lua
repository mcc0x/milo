-- Run with Lua 5.1 against listener.lua from a saved layout with new shell tabs on.
local handlers, timers, splits, windows, processes = {}, {}, {}, {}, {}
local saves, revision = {}, 1
local measured = { rows = 50, owner = "app", desired = {{ client_id = "app", rows = 50 }} }
local sizes, process_reads = {}, 0
local function terminal(method, args)
  assert(args.session_id == "project")
  if method == "size" then return sizes[args.block_id] or measured end
  assert(method == "process")
  process_reads = process_reads + 1
  return processes[args.block_id]
end
local remembered, on_profile, executable, attached
-- A stand-in for `milo tab-title`, which names a project from its directory.
local titling = { on = false, claims = {}, asks = 0, forgotten = {}, relabels = {},
  names = { ["/my project"] = "project", ["/other"] = "other/feature" } }
local function tab_title(argv)
  if argv[3] == "owned" then
    local lines = {}
    for id, title in pairs(titling.claims) do lines[#lines + 1] = id .. "\t" .. title .. "\n" end
    return { ok = true, stdout = table.concat(lines) }
  elseif argv[3] == "forget" then
    if titling.forgetting then titling.forgetting() end
    for i = 4, #argv do titling.claims[argv[i]], titling.forgotten[argv[i]] = nil, true end
    return { ok = true, stdout = "" }
  elseif argv[3] == "claim" then
    assert(#argv == 5 and argv[5] ~= "")
    titling.claims[argv[4]], titling.claimed = argv[5], (titling.claimed or 0) + 1
    return { ok = true, stdout = "" }
  end
  assert(argv[3] == "title" and #argv <= 5)
  titling.asks = titling.asks + 1
  if titling.during then titling.during() end
  if titling.broken then return { ok = false, stdout = "" } end
  local title = titling.on and titling.names[argv[4]] or ""
  if title ~= "" and argv[5] then title = title .. " · " .. argv[5] end
  -- Milo names the files its answer depends on, as the stand-in does once it has one.
  return { ok = true, stdout = "=" .. title .. "\n" .. (titling.grounds and titling.grounds .. "\n" or "") }
end
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
    set_window_label = function(args)
      assert(args.session_id == "project")
      if args.label == titling.refused then
        titling.refusals = (titling.refusals or 0) + 1
        return nil, "bad_request"
      end
      for _, window in ipairs(windows) do
        if window.window_id == args.window_id then
          window.label = args.label ~= "" and args.label or "Window " .. args.window_id:sub(-8)
          titling.relabels[#titling.relabels + 1] = window.label
          return { revision = revision }
        end
      end
    end,
    set_label = function() error("the listener must never name a session") end,
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
    if argv[2] == "tab-title" then return tab_title(argv) end
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
-- foreground is the same shell, whether it is met before or after the shell
-- itself, and with or without the ancestors Rex leaves out when one has just
-- exited. A login shell that another program started in the tab is not.
local login = { pid = 1, path = "/usr/bin/login" }
local own = { pid = 2, argv0 = "-zsh", path = "/bin/zsh", cwd = "/my project" }
local copy = { pid = 4, argv0 = "-zsh", path = "/bin/zsh", cwd = "/my project" }
local at_prompt = { child = login, foreground = own, ancestors = { login } }
local nested = { { pid = 3, argv0 = "su", path = "/usr/bin/su" }, own, login }
for _, case in ipairs({
  { at_prompt, { child = login, foreground = copy, ancestors = { own, login } }, 1 },
  { { child = login, foreground = copy }, at_prompt, 1 },
  { at_prompt, { child = login, foreground = copy, ancestors = nested }, 0 },
}) do
  splits, processes.slow, sizes.slow = {}, case[1], { rows = 24 }
  created("slow")
  processes.slow, sizes.slow = case[2], measured
  emit("terminal.size_changed", "slow")
  assert(#splits == case[3])
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

-- Tab titles. The stand-in terminal answers only `size` and `process`, so a
-- listener that read a terminal's title, screen or status would fail here.
local function titled_tab(n, label, ...)
  local window = tab(string.format("window:00000000-0000-0000-0000-%012x", n), ...)
  window.label = label or string.format("Window %08x", n)
  return window
end
local function shell_in(cwd, agent)
  local zsh = { argv0 = "-zsh", name = "zsh", cwd = cwd }
  if not agent then return { foreground = zsh } end
  return { foreground = { argv0 = agent, name = agent, cwd = cwd }, ancestors = { zsh } }
end
local function front(block, process)
  processes[block] = process
  handlers["terminal.process_changed"]({ session_id = "project", block_id = block },
    { block_id = block, foreground = process.foreground, ancestors = process.ancestors })
end
local function moved(block, cwd)
  processes[block] = shell_in(cwd)
  emit("terminal.pwd_changed", block)
end

-- Until titles are chosen, Milo asks once and leaves the tab as Rex labelled it.
local one = titled_tab(1, nil, "one")
windows, processes, timers, splits = { one }, { one = shell_in("/my project") }, {}, {}
boot(loadfile(arg[1]))
assert(titling.asks == 1 and #titling.relabels == 0 and not next(titling.claims))

-- Once chosen, the title follows the project and the agent in front, and
-- nothing is asked again while both stay the same. A process change carries
-- what Milo needs, so the terminal is not asked about it either.
titling.on = true
local reads = process_reads
front("one", shell_in("/my project", "claude"))
assert(one.label == "project · claude" and titling.claims[one.window_id] == one.label)
changed()
front("one", shell_in("/my project", "claude"))
assert(titling.asks == 2 and #titling.relabels == 1 and titling.claimed == 1 and process_reads == reads)
front("one", shell_in("/my project"))
assert(one.label == "project")
moved("one", "/other")
assert(one.label == "other/feature")

-- Outside a project the tab gets Rex's label back and Milo remembers nothing.
moved("one", "/tmp")
assert(one.label == "Window 00000001" and not titling.claims[one.window_id])
moved("one", "/my project")
changed()
assert(one.label == "project")

-- A name chosen by someone else ends Milo's part: no title, no question.
one.label = "notes"
changed()
assert(titling.forgotten[one.window_id] and not titling.claims[one.window_id])
local asks = titling.asks
front("one", shell_in("/my project", "codex"))
moved("one", "/other")
assert(one.label == "notes" and titling.asks == asks)

-- Clearing that name hands the tab back.
one.label = "Window 00000001"
changed()
assert(one.label == "other/feature")

-- If Milo cannot answer, the title it set earlier stays.
titling.broken = true
moved("one", "/tmp")
assert(one.label == "other/feature")
titling.broken = nil

-- A rename that lands while Milo is answering is kept.
titling.during = function() titling.during, one.label = nil, "mine" end
moved("one", "/my project")
assert(one.label == "mine" and not titling.claims[one.window_id])
one.label = "Window 00000001"
changed()
front("one", shell_in("/my project", "omp"))
assert(one.label == "project · omp")

-- So is a name chosen while Milo is busy with another tab, though the view
-- Milo is working from still shows that tab without one.
local busy = titled_tab(8, nil, "busy")
processes.busy, windows[2] = shell_in("/other"), busy
changed()
assert(busy.label == "other/feature")
one.label, busy.label = "build", "Window 00000008"
titling.forgetting = function() titling.forgetting, busy.label = nil, "mine" end
changed()
assert(one.label == "build" and busy.label == "mine")
one.label, windows[2] = "Window 00000001", nil
changed()
assert(one.label == "project · omp")

-- A tab that closes while Milo is answering is left as it was.
titling.during, titling.relabels = function() titling.during, windows = nil, {} end, {}
moved("one", "/other")
windows = { one }
assert(#titling.relabels == 0 and titling.claims[one.window_id] == "project · omp")

-- A moment without a known directory does not take the title away.
processes.one = { foreground = { argv0 = "login", name = "login" } }
emit("terminal.process_changed", "one")
assert(one.label == "project · omp")

-- A label Rex refuses is tried once, not on every later event.
titling.refused = "other/feature"
moved("one", "/other")
changed()
changed()
assert(one.label == "project · omp" and titling.refusals == 1 and titling.claims[one.window_id] == one.label)
front("one", shell_in("/other", "omp"))
assert(one.label == "other/feature · omp")

-- An agent in any shell of the tab names its own project. Milo's panes are not shells.
local two = titled_tab(2, nil, "left", "right", "rail")
two.layers[1].blocks[3].label = "milo:outline"
processes.left, processes.right = shell_in("/tmp"), shell_in("/other", "claude")
processes.rail = shell_in("/my project", "codex")
windows[2] = two
changed()
assert(two.label == "other/feature · claude")

-- A shell moved to another tab takes its agent along, and is read afresh on its return.
local three = titled_tab(7, nil, "spare")
processes.spare = shell_in("/tmp")
table.insert(three.layers[1].blocks, table.remove(two.layers[1].blocks, 2))
windows[3] = three
changed()
assert(two.label == "Window 00000002" and three.label == "other/feature · claude")
front("right", shell_in("/other"))
assert(three.label == "Window 00000007")
table.insert(two.layers[1].blocks, 2, table.remove(three.layers[1].blocks, 2))
changed()
assert(two.label == "Window 00000002")
front("right", shell_in("/other", "codex"))
assert(two.label == "other/feature · codex")
front("right", shell_in("/other"))
windows[3] = nil
changed()
assert(two.label == "Window 00000002")

-- Closing a tab forgets the title Milo gave it, and asks nothing about one it never titled.
windows[2], titling.forgotten = nil, {}
changed()
assert(not next(titling.forgotten))
processes.right = shell_in("/other", "claude")
windows[2] = two
changed()
assert(two.label == "other/feature · claude")
windows[2] = nil
changed()
assert(titling.forgotten[two.window_id] and titling.claims[one.window_id])

-- A new session's first tab is titled too, though no view event announces it.
local first = titled_tab(6, nil, "first")
windows, processes = { first }, { first = shell_in("/my project") }
handlers.session_destroyed({}, { session_id = "project" })
handlers.session_created({}, { session = { session_id = "project" } })
assert(first.label == "project")

-- A restarted listener goes on titling only the tabs that still carry the
-- title Milo set. A changed one is left alone, like one Milo never titled.
local kept, renamed, foreign = titled_tab(3, "project", "kept"), titled_tab(4, "renamed", "renamed"),
  titled_tab(5, "project", "foreign")
titling.claims = { [kept.window_id] = "project", [renamed.window_id] = "project" }
windows, titling.relabels, asks = { kept, renamed, foreign }, {}, titling.asks
processes = { kept = shell_in("/my project", "omp"), renamed = shell_in("/my project", "omp"),
  foreign = shell_in("/my project", "omp") }
boot(loadfile(arg[1]))
assert(kept.label == "project · omp" and renamed.label == "renamed" and foreign.label == "project")
assert(#titling.relabels == 1 and titling.asks == asks + 1 and not titling.claims[renamed.window_id])

-- Turning titles off gives a tab Rex's label back the next time it changes.
titling.on = false
front("kept", shell_in("/my project"))
assert(kept.label == "Window 00000003" and not next(titling.claims))

-- A destroyed session takes its remembered titles with it.
titling.on = true
front("kept", shell_in("/my project", "claude"))
assert(titling.claims[kept.window_id] == "project · claude")
handlers.session_destroyed({}, { session_id = "project" })
assert(not next(titling.claims))

-- With titles off, a directory Milo has answered for starts nothing more: not
-- for another tab in it, a shell that comes back to it, or an agent starting.
local choice = os.tmpname()
local function choose(text)
  local out = assert(io.open(choice, "w"))
  out:write(text)
  out:close()
end
choose("")
local here, there = titled_tab(9, nil, "here"), titled_tab(10, nil, "there")
titling.on, titling.grounds, titling.relabels = false, choice, {}
windows, processes, asks = { here, there }, { here = shell_in("/my project"), there = shell_in("/my project") }, titling.asks
boot(loadfile(arg[1]))
assert(titling.asks == asks + 1)
moved("here", "/other")
moved("here", "/my project")
front("there", shell_in("/my project", "claude"))
assert(titling.asks == asks + 2 and #titling.relabels == 0)

-- Saving the choice is noticed on the next event, even at the same cwd and agent.
titling.on = true
choose("tab_titles = true\n")
front("there", shell_in("/my project", "claude"))
assert(there.label == "project · claude" and titling.asks == asks + 3)
front("there", shell_in("/my project", "claude"))
assert(titling.asks == asks + 3, "unchanged grounds must not start Milo")
changed()
assert(here.label == "project" and titling.asks == asks + 4)
choose("")
titling.on = false
changed()
assert(here.label == "Window 00000009" and there.label == "Window 0000000a" and titling.asks == asks + 5)
titling.on = true
assert(os.remove(choice))

-- A new tab is arranged and titled by the same events, each from what it
-- knows of the tab's shell. While the terminal is still a copy of the Rex
-- server the tab gets neither a rail nor a title. Its shell then brings the
-- title, and a copy of that shell in front when the tab is measured does not
-- cost the tab its rail or change its title.
local fresh = titled_tab(11, nil, "fresh")
titling.grounds, titling.relabels, splits, remembered = nil, {}, {}, nil
windows, processes, sizes = { fresh }, { fresh = starting }, { fresh = { rows = 24 } }
changed()
created(fresh.window_id)
assert(#splits == 0 and fresh.label == "Window 0000000b")
front("fresh", at_prompt)
assert(#splits == 0 and fresh.label == "project")
processes.fresh, sizes.fresh = { child = login, foreground = copy }, measured
emit("terminal.size_changed", "fresh")
assert(#splits == 1 and splits[1].anchor_id == "fresh")
assert(fresh.label == "project" and #titling.relabels == 1 and titling.claims[fresh.window_id] == "project")
