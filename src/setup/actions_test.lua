-- With Lua 5.1: lua src/setup/actions_test.lua src/setup/actions.lua
local file = assert(io.open(assert(arg[1]), "r"))
local code = file:read("*a")
file:close()
local prefix = [[
local milo, column_width, outline_share, usage_rows = "milo", 0.2, 0.7, 10
local chosen = { outline = true, history = true, usage = true }
local trial_profile = "v1 l 0.37 0.7 10 huo 110 230000 770000 0"
]]
local terminal = "com.superlogical.terminal"
local shell = { block_id = "block:shell", creator_name = terminal }
local editor = { block_id = "block:editor", creator_name = "editor" }
local outline = { block_id = "block:outline", label = "milo:outline", creator_name = terminal }
local usage = { block_id = "block:usage", label = "milo:usage", creator_name = terminal }
local history = { block_id = "block:history", label = "milo:history", creator_name = terminal }
local function leaf(block)
  return { block_id = block.block_id, rect = {0, 0, 1, 1} }
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
rex = {
  action = function(action) actions[action.name] = action.run end,
  exec = function(argv)
    commands[#commands + 1] = argv
    return { ok = true, stdout = "" }
  end,
  call = function(name, args)
    if name == terminal .. ".process" then
      assert(args.block_id == shell.block_id)
      return { foreground = { argv0 = "editor", cwd = "/unrelated" },
        ancestors = { { argv0 = "-zsh", cwd = "/my project" } } }
    elseif name == "block.close" then
      assert(args.keep_last)
      closed[#closed + 1] = args.block_id
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
assert(argv[6] == "v1 r 0.320000 0.7 10 uho 111 438000 292000 270000")
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

local function probe(args)
  rex.args, rex.session_id, rex.block_id = args, ctx.session_id, ctx.block_id
  local output, saved_print = nil, print
  print = function(s) output = s end
  assert(loadstring(prefix .. code))()
  print = saved_print
  return output
end

-- An existing rail is safe: trial appears separately without moving or closing it.
assert(probe{ milo_operation = "trial" } == "ok window:work floating history=block:trial-history outline=block:trial-outline")
assert(#floats == 2 and #splits == 0 and not floats[2].focus)
assert(floats[2].bounds[1] == 0 and floats[2].bounds[3] == 0.37)
assert(floats[2].layout.before.label == "milo:history" and math.abs(floats[2].layout.ratio - 0.77) < 0.000001)

-- Undo honors the original window and exact IDs: it never closes the actual outline,
-- a user shell added to the preview, or a trial block the user moved to another tab.
window.layers[2] = { kind = "floating", blocks = {
  { block_id = "block:trial-history", creator_name = terminal, label = "milo:history" },
  { block_id = "block:new-shell", creator_name = terminal },
} }
session.windows[2] = { window_id = "window:elsewhere", layers = {{ kind = "tiled", blocks = {
  { block_id = "block:trial-outline", creator_name = terminal, label = "milo:outline" },
} }} }
assert(probe{ milo_operation = "undo", window = "window:work", history = "block:trial-history", outline = "block:trial-outline" } == "ok")
assert(#closed == 2 and closed[2] == "block:trial-history")

-- On a pristine tab, Try can place the full draft without taking keyboard focus.
window.layers[2] = nil
tiled.layout, tiled.blocks = leaf(shell), { shell }
assert(probe{ milo_operation = "trial" } == "ok window:work tiled history=block:trial-history outline=block:trial-outline")
assert(#splits == 1 and splits[1].anchor_id == shell.block_id and not splits[1].focus)
