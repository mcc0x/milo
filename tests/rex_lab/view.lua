-- Prints what tests/rex_lab.rs checks on its disposable server, one
-- tab-separated line per session and per window:
--   session <id> <label>
--   window <session id> <window id> <label> <left column's share or -> <pane>...
-- A pane is <block id>|<label>|<milo, shell or other>|<directory or ->, in the
-- order the tab lays its panes out. Pass milo=PATH to name the binary under test.
local terminal = "com.superlogical.terminal"
local milo = rex.args and rex.args.milo

local function pane(session_id, block)
  local kind, cwd = "other", nil
  if block.creator_name == terminal then
    local process = rex.call(terminal .. ".process", { session_id = session_id, block_id = block.block_id })
    local child, foreground = process and process.child, process and process.foreground
    if child and (child.path == milo or child.invoked_path == milo or child.argv0 == milo) then
      kind, cwd = "milo", child.cwd
    elseif foreground and foreground.argv0 and foreground.argv0:sub(1, 1) == "-" then
      kind, cwd = "shell", foreground.cwd
    end
  end
  return table.concat({ block.block_id, block.label or "", kind, cwd or "-" }, "|")
end

local listed = rex.call("session.list")
for _, session in ipairs(listed and listed.sessions or {}) do
  local id = session.session_id
  -- A script started without a session must attach before it can read one.
  rex.session.attach{ session_id = id }
  local view = rex.session.view{ session_id = id }
  print(table.concat({ "session", id, view and view.label or "" }, "\t"))
  for _, window in ipairs(view and view.windows or {}) do
    local fields = { "window", id, window.window_id, window.label or "", "-" }
    for _, layer in ipairs(window.layers or {}) do
      local blocks = {}
      for _, block in ipairs(layer.blocks or {}) do blocks[block.block_id] = block end
      local function walk(node)
        if not node then return end
        if node.block_id and blocks[node.block_id] then
          fields[#fields + 1] = pane(id, blocks[node.block_id])
        end
        local split = node.split
        if split then walk(split.before); walk(split.after) end
      end
      if layer.kind == "tiled" then
        local root = layer.layout and layer.layout.split
        if root and root.direction == "horizontal" then fields[5] = string.format("%.6f", root.ratio) end
      end
      walk(layer.layout)
    end
    print(table.concat(fields, "\t"))
  end
end
