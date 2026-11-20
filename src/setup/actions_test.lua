-- With Lua 5.1: lua src/setup/actions_test.lua src/setup/actions.lua
local file = assert(io.open(assert(arg[1]), "r"))
local code = file:read("*a")
file:close()
local prefix = [[
local milo, column_width, outline_share, usage_rows = "/test/bin/milo", 0.2, 0.7, 2
local chosen = { outline = true, history = true, usage = true }
local trial_profile = "v1 l 0.37 0.7 10 huo 110 230000 770000 0"
]]
local terminal = "com.superlogical.terminal"
local shell = { block_id = "block:shell", creator_name = terminal }
local editor = { block_id = "block:editor", creator_name = "editor" }
local outline = { block_id = "block:outline", label = "milo:outline", creator_name = terminal }
local usage = { block_id = "block:usage", label = "milo:usage", creator_name = terminal }
local history = { block_id = "block:history", label = "milo:history", creator_name = terminal }
for _, block in ipairs{ outline, usage, history } do
  block.flavor, block.host_id = terminal .. ".shell", "host:local"
end
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
local session = { active_window_id = window.window_id, windows = { window }, revision = 1 }
local ctx = { session_id = "session:1", block_id = shell.block_id }
local actions, commands, closed, focus, floats, splits = {}, {}, {}, {}, {}, {}
local sizes, profile_reply = {}, { stdout = "", ok = true }
local relabeled = {}
local named, title_reply, rename_reply, rename_error = {}, { ok = true, stdout = "" }, { revision = 2 }, nil
local running = { child = { path = "/test/bin/milo", pid = 80 } }
local processes = { [outline.block_id] = running, [history.block_id] = running, [usage.block_id] = running }
local pick_answer, on_close, ps_reply
rex = {
  action = function(action) actions[action.name] = action.run end,
  exec = function(argv)
    if argv[1] == "/bin/ps" then return ps_reply end
    commands[#commands + 1] = argv
    if argv[2] == "tab-title" then return title_reply end
    return argv[3] == "read" and profile_reply or { ok = true, stdout = "" }
  end,
  call = function(name, args)
    if name == terminal .. ".process" then
      if args.block_id ~= shell.block_id then return processes[args.block_id] end
      return { foreground = { argv0 = "editor", cwd = "/unrelated" },
        ancestors = { { argv0 = "-zsh", cwd = "/my project" } } }
    elseif name == terminal .. ".size" then
      return sizes[args.block_id]
    elseif name == "block.close" then
      assert(args.keep_last)
      closed[#closed + 1] = args.block_id
      if on_close then on_close(args.block_id) end
    elseif name == "server.status" then
      return { host_id = "host:local" }
    elseif name == "session.create" or name == "session.destroy" or name == "session.set_label" then
      error("an action changed a session through " .. name)
    end
    return {}
  end,
  ui = { pick = function(args) return assert(pick_answer, "unexpected picker")(args) end },
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
    set_label = function(args)
      named[#named + 1] = args
      return rename_reply, rename_error
    end,
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

-- Milo names the files an answer depends on. While they read the same, the
-- directory is not asked about again, whichever tab it is in. Without such
-- files, as above, every question reaches Milo.
local asked = 0
for _, command in ipairs(commands) do
  if command[3] == "title" then asked = asked + 1 end
end
-- Whether Milo was started `times` times to answer since the last check.
local function started(times)
  local count = 0
  for _, command in ipairs(commands) do
    if command[3] == "title" then count = count + 1 end
  end
  count, asked = count - asked, count
  return count == times
end
local function write(path, text)
  local out = assert(io.open(path, "w"))
  out:write(text)
  out:close()
end
local ask, global, override = listener.titles.ask, os.tmpname(), os.tmpname()
local grounds = "\n" .. global .. "\n" .. override .. "\n"
write(global, "")
os.remove(override)
title_reply.stdout = "=" .. grounds
assert(ask("/off") == "" and ask("/off") == "" and ask("/off", "claude") == "" and started(1))

-- A choice saved for one worktree is noticed, in a file that was not there. With
-- titles on, the answer differs by agent, and each is asked for once.
write(override, "tab_titles = true\n")
title_reply.stdout = "=milo" .. grounds
assert(ask("/off", nil, true) == "milo" and started(1))
title_reply.stdout = "=milo · omp" .. grounds
assert(ask("/off", "omp") == "milo · omp" and started(1))
assert(ask("/off") == "milo" and ask("/off", "omp") == "milo · omp" and started(0))

-- So is a change to the global choice, and the override being taken away.
write(global, "tab_titles = false\n")
title_reply.stdout = "=milo · codex" .. grounds
assert(ask("/off", "codex") == "milo · codex" and started(1))
os.remove(override)
title_reply.stdout = "=" .. grounds
assert(ask("/off", "codex") == "" and ask("/off") == "" and started(1))

-- An answer Milo could not give is not kept.
title_reply.ok = false
assert(ask("/failing") == nil and started(1))
title_reply.ok = true
assert(ask("/failing") == "" and ask("/failing") == "" and started(1))

-- Nor is one without grounds, as when Git could not run. It is passed on that
-- once and takes nothing from what is kept for the directory.
write(override, "tab_titles = true\n")
title_reply.stdout = "=milo · omp" .. grounds
assert(ask("/kept", "omp") == "milo · omp" and started(1))
title_reply.stdout = "=\n"
assert(ask("/uncached") == "" and started(1))
assert(ask("/uncached", nil, true) == nil and started(0))
assert(ask("/kept") == "" and started(1))
assert(ask("/kept", "omp") == "milo · omp" and started(0))
title_reply.stdout = "=milo" .. grounds
assert(ask("/kept") == "milo" and started(1))

-- If the directory turns out to depend on other files, as inside a new
-- repository of its own, what was kept for the old ones goes.
title_reply.stdout = "=inner · codex\n" .. global .. "\n"
assert(ask("/kept", "codex") == "inner · codex" and started(1))
title_reply.stdout = "=inner · omp\n" .. global .. "\n"
assert(ask("/kept", "omp") == "inner · omp" and started(1))
assert(ask("/kept", "codex") == "inner · codex" and ask("/kept", "omp") == "inner · omp" and started(0))
os.remove(override)

-- Outside a project the answer stands until a repository appears, as a folder too.
title_reply.stdout = "=\n" .. override .. "/HEAD\n" .. override .. "\n"
assert(ask("/plain") == "" and ask("/plain", "omp") == "" and started(1))
assert(os.execute("mkdir '" .. override .. "'") == 0)
assert(ask("/plain") == "" and started(1))
assert(os.remove(override) and os.remove(global))

-- Nothing above named a session: not another action, nor the part of the listener kept in this file.
assert(#named == 0)

-- The explicit action names the session it was run in for its shell's project.
-- It makes no session, and asks no app to show one.
tiled.layout, tiled.blocks = leaf(shell), { shell }
title_reply.stdout = "milo/rex-titles\n"
rex.client = { queue = function() error("the action asked an app to do something") end }
local app = { session_id = "session:other", block_id = ctx.block_id, client_id = "client:app", origin = "palette" }
assert(actions.milo_name_session(app) == "Named this session milo/rex-titles.")
argv = commands[#commands]
assert(argv[2] == "tab-title" and argv[3] == "name" and argv[4] == "/my project" and #argv == 4)
assert(#named == 1 and named[1].session_id == "session:other" and named[1].label == "milo/rex-titles")
actions.milo_name_session(ctx)
assert(#named == 2 and named[2].session_id == ctx.session_id)

-- Outside a Git project the session keeps its name, and the action passes on Milo's reason.
title_reply.ok, title_reply.stderr = false, "This shell is not in a Git project.\n"
ok, why = pcall(actions.milo_name_session, ctx)
assert(not ok and why == "This shell is not in a Git project." and #named == 2)
title_reply.ok, title_reply.stdout, title_reply.stderr = true, "two\nnames\n", nil
ok, why = pcall(actions.milo_name_session, ctx)
assert(not ok and why == "Could not name this session." and #named == 2)

-- If Rex refuses the name, the action says so.
title_reply.stdout, rename_reply, rename_error = "milo\n", nil, "not_found"
ok, why = pcall(actions.milo_name_session, ctx)
assert(not ok and why == "not_found" and #named == 3)

local function mac_shell(pid, login)
  local wrapper = { pid = login, name = "login", path = "/usr/bin/login", user_id = 0 }
  return { child = wrapper, ancestors = { wrapper },
    foreground = { pid = pid, user_id = 501, name = "zsh", argv0 = "-zsh", path = "/bin/zsh", cwd = "/my project" } }
end
local linux_shell = { pid = 33, user_id = 501, name = "bash", argv0 = "-bash",
  path = "/usr/bin/bash", cwd = "/my project" }
local function restore_rail()
  tiled.layout, tiled.blocks = saved_rail, { shell, editor, usage, history, outline }
  processes[outline.block_id] = mac_shell(11, 10)
  processes[history.block_id] = mac_shell(21, 20)
  processes[usage.block_id] = { foreground = linux_shell, child = linux_shell }
  ps_reply = { ok = true, stdout = "10 1 ttys000\n11 10 ttys000\n20 1 ttys001\n21 20 ttys001\n33 1 pts/2\n" }
end

restore_rail()
local close_count, add_count = #closed, #splits
assert(observe() == "restored") -- Doctor is read-only and does not compare shell sizes.
local function aborted(message, action)
  local done, err = pcall(actions[action or "milo_layout"], ctx)
  assert(not done and err:find(message, 1, true), tostring(err))
  assert(#closed == close_count and #splits == add_count)
end
local picked = 0
local function inspect_prompt(prompt)
  picked = picked + 1
  assert(prompt.title == "Rebuild Milo sidebar?")
  assert(prompt.items[1].id == "keep" and prompt.items[2].id == "rebuild")
  assert(prompt.items[2].subtitle:find("Closes 3 restored shells", 1, true))
  assert(prompt.items[2].subtitle:find("discards their input and scrollback", 1, true))
end
pick_answer = function(prompt) inspect_prompt(prompt); return nil, "cancelled" end
actions.milo_layout(ctx)
pick_answer = function(prompt) inspect_prompt(prompt); return "keep" end
actions.milo_layout(ctx)
pick_answer = function(prompt) inspect_prompt(prompt); return nil, "no_client" end
aborted("in a connected Rex app to confirm")
aborted("Run Milo: Arrange This Tab to rebuild", "milo_outline")
assert(picked == 3)

-- A busy, nested, unreadable, or mixed rail does not ask to close anything.
local idle_outline = processes[outline.block_id]
local function unsupported(process)
  processes[outline.block_id] = process
  assert(observe() == "unverified")
  aborted("could not verify what is running")
  assert(picked == 3)
end
unsupported(nil)
unsupported(running)
unsupported{ child = idle_outline.child, ancestors = { idle_outline.foreground, idle_outline.child },
  foreground = { pid = 12, name = "sleep", path = "/bin/sleep", argv0 = "sleep", cwd = "/my project", user_id = 501 } }
unsupported{ child = idle_outline.child, ancestors = { idle_outline.foreground, idle_outline.child },
  foreground = { pid = 12, name = "zsh", path = "/bin/zsh", argv0 = "-zsh", cwd = "/my project", user_id = 501 } }
processes[outline.block_id] = idle_outline
outline.flavor = terminal .. ".other"
aborted("could not verify what is running")
outline.flavor = terminal .. ".shell"
outline.host_id = "host:remote"
aborted("could not verify that the restored shells are idle")
outline.host_id = "host:local"

-- Rex still reports the shell in the foreground with a background job.
local quiet_ps = ps_reply.stdout
ps_reply.stdout = quiet_ps .. "22 21 ??\n"
aborted("could not verify that the restored shells are idle")
ps_reply.stdout = quiet_ps .. "22 1 ttys001\n"
aborted("could not verify that the restored shells are idle")
ps_reply.stdout = quiet_ps
assert(picked == 3)

-- Recheck the identity, membership, and any newly started jobs after the picker yields.
pick_answer = function() processes[outline.block_id] = mac_shell(51, 50); return "rebuild" end
aborted("a restored shell changed")
processes[outline.block_id] = idle_outline
pick_answer = function() outline.label = "user shell"; return "rebuild" end
aborted("a restored shell changed")
outline.label = "milo:outline"
pick_answer = function() ps_reply.stdout = quiet_ps .. "22 21 ttys001\n"; return "rebuild" end
aborted("could not verify that the restored shells are idle")
ps_reply.stdout = quiet_ps

local function remove(node, id)
  if not node.split then
    if node.block_id ~= id then return node end
    return
  end
  local split = node.split
  local before, after = remove(split.before, id), remove(split.after, id)
  if not before then return after end
  if not after then return before end
  return tree(split.direction, split.ratio, before, after)
end
on_close = function(id)
  tiled.layout = remove(tiled.layout, id)
  for i, block in ipairs(tiled.blocks) do
    if block.block_id == id then table.remove(tiled.blocks, i); break end
  end
  session.revision = session.revision + 1
end

-- Confirmation replaces only those three shells and rebuilds against the
-- preserved work area, even if the action was run from a shell being closed.
pick_answer = function(prompt)
  inspect_prompt(prompt)
  profile_reply.stdout = "v1 r 0.32 0.7 2 uho 111 438000 292000 270000"
  return "rebuild"
end
actions.milo_layout{ session_id = ctx.session_id, block_id = usage.block_id }
assert(#closed == close_count + 3 and #splits == add_count + 1)
assert(closed[close_count + 1] == usage.block_id and closed[close_count + 2] == history.block_id
  and closed[close_count + 3] == outline.block_id)
assert(#tiled.blocks == 2 and tiled.blocks[1] == shell and tiled.blocks[2] == editor)
made = splits[#splits]
assert(made.anchor_id == work.split.split_id and made.side == "after")
near(1 - made.ratio, .32)
near(1 - made.layout.ratio, .438)
near(made.layout.before.ratio * made.layout.ratio, .27)
assert(made.layout.before.before.label == usage.label and made.layout.before.after.label == history.label
  and made.layout.after.label == outline.label)
