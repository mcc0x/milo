-- With Lua 5.1: lua src/setup/actions_test.lua src/setup/actions.lua
local file = assert(io.open(assert(arg[1]), "r"))
local code = file:read("*a")
file:close()
local prefix = [[
local milo, column_width, outline_share, usage_rows = "milo", 0.2, 0.7, 2
local chosen = { outline = true, history = true, usage = true }
local trial_profile = "v1 l 0.37 0.7 10 huo 110 230000 770000 0"
]]
local terminal = "com.superlogical.terminal"
local shell = { block_id = "block:shell", creator_name = terminal }
local editor = { block_id = "block:editor", creator_name = "editor" }
local outline = { block_id = "block:outline", label = "milo:outline", creator_name = terminal }
local usage = { block_id = "block:usage", label = "milo:usage", creator_name = terminal }
local history = { block_id = "block:history", label = "milo:history", creator_name = terminal }
local function leaf(block, rect)
  return { block_id = block.block_id, rect = rect or {0, 0, 1, 1} }
end
local function tree(direction, ratio, before, after)
  return { split = { split_id = "split:root", direction = direction, ratio = ratio, before = before, after = after } }
end
local rail = tree("vertical", 0.27, leaf(usage), tree("vertical", 0.4, leaf(history), leaf(outline)))
local work = tree("vertical", 0.6, leaf(shell), leaf(editor))
local tiled = { kind = "tiled", layout = tree("horizontal", 0.68, work, rail), blocks = { shell, editor, usage, history, outline } }
local window = { window_id = "window:work", focused_block_id = shell.block_id, layers = { tiled } }
local session = { active_window_id = window.window_id, windows = { window } }
local ctx = { session_id = "session:1", block_id = shell.block_id }
local actions, commands, closed, focus, floats, splits = {}, {}, {}, {}, {}, {}
local sizes, profile_reply = {}, { stdout = "", ok = true }
local relabeled = {}
local created, title_reply, create_reply = {}, { ok = true, stdout = "" }, { session_id = "session:new" }
rex = {
  action = function(action) actions[action.name] = action.run end,
  exec = function(argv)
    commands[#commands + 1] = argv
    if argv[2] == "tab-title" then return title_reply end
    return argv[3] == "read" and profile_reply or { ok = true, stdout = "" }
  end,
  call = function(name, args)
    if name == terminal .. ".process" then
      assert(args.block_id == shell.block_id)
      return { foreground = { argv0 = "editor", cwd = "/unrelated" },
        ancestors = { { argv0 = "-zsh", cwd = "/my project" } } }
    elseif name == terminal .. ".size" then
      return sizes[args.block_id]
    elseif name == "block.close" then
      assert(args.keep_last)
      closed[#closed + 1] = args.block_id
    elseif name == "session.create" then
      created[#created + 1] = args
      return create_reply
    elseif name == "session.set_label" then
      error("an action renamed a session")
    end
    return {}
  end,
  session = {
    view = function() return session end,
    new_split = function(args)
      splits[#splits + 1] = args
      return { block_ids = { "block:trial-history", "block:trial-outline" } }
    end,
    new_layer = function(args)
      floats[#floats + 1] = args
      local ids = { "block:trial-history", "block:trial-outline" }
      if args.layout.label == "milo:peek:outline" then
        local block = { block_id = "block:peek", label = args.layout.label, creator_name = terminal }
        ids = { block.block_id }
        table.insert(window.layers, { kind = "floating", layer_id = "layer:peek", blocks = { block } })
        window.focused_block_id = block.block_id
      end
      return { block_ids = ids }
    end,
    raise_layer = function() return {} end,
    focus_block = function(args) focus[#focus + 1] = args.block_id; return {} end,
    set_block_label = function(args) relabeled[#relabeled + 1] = args; return {} end,
  },
  layout = {
    block = function(block) return block end,
    vertical = function(ratio, before, after) return { ratio = ratio, before = before, after = after } end,
  },
}
assert(loadstring(prefix .. code))()

-- Commit geometry, not Rex IDs or unrelated work-area dimensions.
actions.milo_remember_layout(ctx, { scope = "worktree" })
local argv = commands[1]
assert(argv[2] == "layout-profile" and argv[3] == "save" and argv[4] == "worktree" and argv[5] == "/my project")
assert(argv[6] == "v1 r 0.320000 0.7 2 uho 111 438000 292000 270000")
actions.milo_reset_layout(ctx, { scope = "worktree" })
assert(commands[2][3] == "reset" and commands[2][4] == "worktree")

-- A user block mixed into the rail must be refused visibly without writing.
tiled.layout = tree("horizontal", 0.68, leaf(shell), tree("vertical", 0.7, rail, leaf(editor)))
local ok, why = pcall(actions.milo_remember_layout, ctx, { scope = "global" })
assert(not ok and why:match("separate from other panes") and #commands == 2)
tiled.layout = tree("horizontal", 0.68, work, rail)

-- Repeated peek reuses the layer, and dismissal only closes it and focuses the origin.
actions.milo_peek_outline(ctx)
assert(#floats == 1 and floats[1].focus and floats[1].layout.options.command[3] == "--peek-origin")
assert(floats[1].layout.options.command[4] == "block:shell" and floats[1].layout.options.cwd == "/my project")
actions.milo_peek_outline(ctx)
assert(#floats == 1 and focus[1] == "block:peek")
actions.milo_close_peek(ctx)
assert(#closed == 1 and closed[1] == "block:peek" and focus[2] == "block:shell")
window.layers[2] = nil

local function probe(args, profile)
  rex.args, rex.session_id, rex.block_id = args, ctx.session_id, ctx.block_id
  local output, saved_print = nil, print
  print = function(s) output = s end
  local declarations = profile and prefix .. "local trial_profile = " .. string.format("%q", profile) .. "\n" or prefix
  assert(loadstring(declarations .. code))()
  print = saved_print
  return output
end

-- Doctor reads the caller's tab and measured geometry without changing it.
local saved_rail, saved_blocks = tiled.layout, tiled.blocks
local doctor_args = { milo_operation = "doctor", caller = shell.block_id }
local function observe(profile)
  return probe(doctor_args, profile or "v1 r 0.32 0.7 2 ohu 101")
end
local function sidebar(share)
  return tree("horizontal", 0.68, leaf(shell),
    tree("vertical", share, leaf(usage, {0.68, 0, 0.32, share}), leaf(outline)))
end
tiled.layout, tiled.blocks = sidebar(0.05), { shell, usage, outline }
sizes[shell.block_id] = { rows = 40, columns = 80, owner = "client:1" }
sizes[usage.block_id] = { rows = 3, columns = 22, owner = "client:1" }
assert(observe() == "ok r 0.32 3 2 ok") -- One row of rounding.
sizes[usage.block_id].rows = 5
assert(observe() == "ok r 0.32 5 2 ok") -- A different font, same divider.
tiled.layout = sidebar(0.25)
sizes[usage.block_id].rows = 10
assert(observe() == "ok r 0.32 10 2 different")
assert(observe("v1 r 0.32 0.7 2 ohu 101 750000 0 250000") == "ok r 0.32 10 10 ok")
sizes[shell.block_id], sizes[usage.block_id] = nil, { rows = 24 }
assert(observe() == "ok r 0.32 - - ok") -- The fallback PTY dimensions aren't live.
window.zoomed_block_id = shell.block_id
assert(observe() == "zoomed")
window.zoomed_block_id = nil
doctor_args.caller = "shell"
assert(observe() == "no_tab")
doctor_args.caller = shell.block_id
session.windows[2] = { window_id = "window:elsewhere", layers = {{ kind = "tiled", layout = tiled.layout,
  blocks = { usage, outline } }} }
session.active_window_id = "window:elsewhere"
window.layers[1] = { kind = "tiled", layout = leaf(shell), blocks = { shell } }
assert(observe() == "no_rail") -- The rail in the active tab does not count.
window.layers[1] = tiled
session.active_window_id, session.windows[2] = window.window_id, nil
assert(#commands == 2 and #splits == 0 and #closed == 1 and #floats == 1 and #focus == 2)
tiled.layout, tiled.blocks, sizes[usage.block_id] = saved_rail, saved_blocks, nil

-- An existing rail is safe: trial appears separately without moving or closing it.
assert(probe{ milo_operation = "trial" } == "ok window:work floating history=block:trial-history outline=block:trial-outline")
assert(#floats == 2 and #splits == 0 and not floats[2].focus)
assert(floats[2].bounds[1] == 0 and floats[2].bounds[3] == 0.37)
assert(floats[2].layout.before.label == "milo:trial:history" and math.abs(floats[2].layout.ratio - 0.77) < 0.000001)

-- Undo honors the original window and exact IDs: it never closes the actual outline,
-- a user shell added to the preview, or a trial block the user moved to another tab.
window.layers[2] = { kind = "floating", blocks = {
  { block_id = "block:trial-history", creator_name = terminal, label = "milo:trial:history" },
  { block_id = "block:new-shell", creator_name = terminal },
} }
session.windows[2] = { window_id = "window:elsewhere", layers = {{ kind = "tiled", blocks = {
  { block_id = "block:trial-outline", creator_name = terminal, label = "milo:trial:outline" },
} }} }
assert(probe{ milo_operation = "undo", window = "window:work", history = "block:trial-history", outline = "block:trial-outline" } == "ok")
assert(#closed == 2 and closed[2] == "block:trial-history")

-- On a pristine tab, Try can place the full draft without taking keyboard focus.
window.layers[2] = nil
tiled.layout, tiled.blocks = leaf(shell), { shell }
assert(probe{ milo_operation = "trial" } == "ok window:work tiled history=block:trial-history outline=block:trial-outline")
assert(#splits == 1 and splits[1].anchor_id == shell.block_id and not splits[1].focus)

-- Saving promotes only trial panes still in their original tab.
assert(splits[1].layout.before.label == "milo:trial:history")
tiled.blocks[2] = { block_id = "block:trial-history", creator_name = terminal, label = "milo:trial:history" }
assert(probe{ milo_operation = "keep", window = "window:work", history = "block:trial-history", outline = "block:trial-outline" } == "ok")
assert(#relabeled == 1 and relabeled[1].block_id == "block:trial-history" and relabeled[1].label == "milo:history")

local function near(a, b)
  assert(math.abs(a - b) < 0.000001, a .. " should be near " .. b)
end

-- The shell fills only 60% of the tab. Its 48 rows imply an 80-row tab.
tiled.layout = tree("vertical", 0.6, leaf(shell, {0, 0, 1, 0.6}), leaf(editor))
tiled.blocks = { shell, editor }
sizes[shell.block_id] = { owner = "client:one", columns = 100, rows = 48 }
actions.milo_layout(ctx)
local made = splits[#splits]
assert(made.direction == "horizontal" and made.layout.after.label == usage.label)
near(1 - made.layout.ratio, 2 / 80)

-- Configured rows and captured resize proportions are intentional overrides.
profile_reply.stdout = "v1 l 0.20 0.70 10 ohu 111"
actions.milo_layout(ctx)
near(1 - splits[#splits].layout.ratio, 10 / 80)
profile_reply.stdout = "v1 l 0.20 0.70 10 ohu 111 400000 250000 350000"
actions.milo_layout(ctx)
near(1 - splits[#splits].layout.ratio, 0.35)
profile_reply.stdout = ""

-- An unsized tab uses the compact nominal estimate, not a 15% strip.
sizes[shell.block_id] = nil
actions.milo_layout(ctx)
near(1 - splits[#splits].layout.ratio, 2 / 35)

-- Add Usage splits the real adjacent pane, even if it was resized.
local resized = tree("vertical", 0.8, leaf(outline), leaf(history))
tiled.layout = tree("horizontal", 0.8, leaf(shell), resized)
tiled.blocks = { shell, outline, history }
sizes[shell.block_id] = { owner = "client:one", columns = 100, rows = 80 }
actions.milo_usage(ctx)
made = splits[#splits]
assert(made.direction == "vertical" and made.anchor_id == history.block_id and made.layout.label == usage.label)
near((1 - made.ratio) * 0.2, 2 / 80)

-- A headless tab can still have an explicit terminal size without a client owner.
tiled.layout, tiled.blocks = leaf(shell), { shell }
sizes[shell.block_id] = { columns = 80, rows = 12 }
actions.milo_layout(ctx)
near(1 - splits[#splits].layout.ratio, 2 / 12)

-- On a tab without a rail, Add Usage creates the sole pane in a side rail.
actions.milo_usage(ctx)
made = splits[#splits]
assert(made.direction == "horizontal" and made.layout.label == usage.label)
near(made.ratio, 0.2)

-- Automatic sizing requires the owner's measurement and rechecks the shell
-- before splitting. A profile saved while the tab waits should still win.
local listener = assert(loadstring("local arrange_new_tabs = true\n" .. prefix .. code))("listener")
local split_count = #splits
local apply = listener.prepare_arrangement(ctx, "/my project")
assert(not apply(function() error("the tab was not measured yet") end))
sizes[shell.block_id] = { owner = "client:one", columns = 100, rows = 50 }
assert(not apply(function() error("claiming ownership is not a measurement") end))
sizes[shell.block_id].desired = {{ client_id = "client:one", rows = 50 }}
assert(not apply(function(owner) assert(owner == "client:one"); return true, true end) and #splits == split_count)
assert(apply(function() return false end) and #splits == split_count)

sizes[shell.block_id] = nil
apply = listener.prepare_arrangement(ctx, "/my project")
assert(not apply(function() error("the tab was not measured yet") end))
profile_reply.stdout = "v1 l 0.20 0.70 4 ohu 111"
sizes[shell.block_id] = { owner = "client:one", columns = 100, rows = 50,
  desired = {{ client_id = "client:one", rows = 50 }} }
assert(apply(function() return true end) and #splits == split_count + 1)
near(1 - splits[#splits].layout.ratio, 4 / 50)

-- An agent is recognised by its process name alone, also behind a path or under
-- a command it started. The project is the shell's directory, not an editor's.
local facts = listener.titles.facts
local zsh = { argv0 = "-zsh", name = "zsh", cwd = "/my project" }
local seen = facts{ foreground = { name = "2.1.0", argv0 = "claude", cwd = "/elsewhere" }, ancestors = { zsh } }
assert(seen.agent == "claude" and seen.cwd == "/my project")
assert(facts{ foreground = { name = "node", invoked_path = "/opt/bin/codex", cwd = "/work" } }.agent == "codex")
seen = facts{ foreground = { name = "git", argv0 = "git", cwd = "/work" },
  ancestors = { { name = "omp", argv0 = "/bin/omp" }, zsh } }
assert(seen.agent == "omp" and seen.cwd == "/my project")
seen = facts{ child = { name = "omp", argv0 = "omp", cwd = "/work" } }
assert(seen.agent == "omp" and seen.cwd == "/work")
seen = facts{ foreground = { name = "claude-notes", argv0 = "vim claude", cwd = "relative" } }
assert(not seen.agent and not seen.cwd and not next(facts(nil)))

-- Milo is asked with a directory and an agent's name. Anything but a title means it could not say.
title_reply.stdout = "=milo · claude\n"
assert(listener.titles.ask("/my project", "claude") == "milo · claude")
argv = commands[#commands]
assert(argv[2] == "tab-title" and argv[3] == "title" and argv[4] == "/my project" and argv[5] == "claude" and #argv == 5)
title_reply.stdout = "=\n"
assert(listener.titles.ask("/tmp") == "" and #commands[#commands] == 4)
title_reply.stdout = "milo\n"
assert(listener.titles.ask("/tmp") == nil)
title_reply.ok, title_reply.stdout = false, "=milo\n"
assert(not listener.titles.ask("/tmp") and not next(listener.titles.owned()))
title_reply.ok, title_reply.stdout = true, "window:0a-1\tmilo\nnoise\nwindow:0b-2\tmilo/rex · omp\n"
local owned = listener.titles.owned()
assert(owned["window:0a-1"] == "milo" and owned["window:0b-2"] == "milo/rex · omp" and not owned.noise)
listener.titles.forget{ "window:0a-1", "window:0b-2" }
argv = commands[#commands]
assert(argv[3] == "forget" and argv[4] == "window:0a-1" and argv[5] == "window:0b-2" and #argv == 5)

-- The explicit action makes a session named for the shell's project and opened
-- at its root. The stand-in refuses to rename a session, and the action never asks.
tiled.layout, tiled.blocks = leaf(shell), { shell }
title_reply.stdout = "milo/rex-titles\n/work/milo rex\n"
local shown, app = {}, { session_id = ctx.session_id, block_id = ctx.block_id, client_id = "client:app", origin = "palette" }
rex.client = { queue = function(name, args) shown[#shown + 1] = { name, args.session_id } end }
assert(actions.milo_session_from_project(app):match("milo/rex%-titles"))
argv = commands[#commands]
assert(argv[2] == "tab-title" and argv[3] == "name" and argv[4] == "/my project" and #argv == 4)
assert(#created == 1 and created[1].label == "milo/rex-titles" and #created[1].initial_windows == 1)
local first = created[1].initial_windows[1].layout
assert(first.flavor == terminal .. ".shell" and first.options.cwd == "/work/milo rex" and not first.options.command)
assert(#shown == 1 and shown[1][1] == "session.select" and shown[1][2] == "session:new")

-- The CLI is a client too, but not one that can show a session.
actions.milo_session_from_project{ session_id = ctx.session_id, block_id = ctx.block_id, client_id = "client:cli", origin = "api" }
assert(#created == 2 and #shown == 1)
rex.client = { queue = function() error("no client") end }
actions.milo_session_from_project(app)
assert(#created == 3)

-- Outside a Git project, or when Rex refuses, the action says so.
title_reply.ok = false
ok, why = pcall(actions.milo_session_from_project, ctx)
assert(not ok and why:match("Git project") and #created == 3)
title_reply.ok, create_reply = true, nil
ok, why = pcall(actions.milo_session_from_project, ctx)
assert(not ok and why:match("could not create"))
