-- Stands in for a connected Rex app on the disposable server that
-- tests/rex_lab.rs starts. A real app claims each terminal it shows and reports
-- the size it measured; the listener waits for that before arranging a tab.
local terminal = "com.superlogical.terminal"
-- The size of the pretend app window in terminal cells.
local columns, rows = 200, 50
local measured = {}

local function measure(session_id)
  local view = rex.session.view{ session_id = session_id }
  local function walk(node)
    if not node then return end
    if node.block_id and node.rect then
      local size = math.max(2, math.floor(columns * node.rect[3] + 0.5)) .. "x"
        .. math.max(1, math.floor(rows * node.rect[4] + 0.5))
      if measured[node.block_id] ~= size then
        measured[node.block_id] = size
        local wide, tall = size:match("(%d+)x(%d+)")
        rex.call("session.own_block", { session_id = session_id, block_ids = { node.block_id } })
        rex.call(terminal .. ".resize", { session_id = session_id, block_id = node.block_id,
          args = { columns = tonumber(wide), rows = tonumber(tall) } })
      end
    end
    local split = node.split
    if split then walk(split.before); walk(split.after) end
  end
  for _, window in ipairs(view and view.windows or {}) do
    for _, layer in ipairs(window.layers or {}) do
      if layer.kind == "tiled" then walk(layer.layout) end
    end
  end
end

rex.on("session_view_changed", function(ctx) measure(ctx.session_id) end)
rex.on("session_created", function(_, event)
  local id = event.session and event.session.session_id
  if id then measure(id) end
end)

-- Sessions cannot be read until the script has attached.
rex.after(0, function()
  local listed = rex.call("session.list")
  for _, session in ipairs(listed and listed.sessions or {}) do measure(session.session_id) end
  print("app ready")
end)
