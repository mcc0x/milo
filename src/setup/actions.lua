local terminal = "com.superlogical.terminal"
local labels = { outline = "milo:outline", history = "milo:history", usage = "milo:usage" }

-- Find the caller's tab and its main tiled pane, even if the caller floats.
local function tab_panes(ctx)
  local view, err = rex.session.view{ session_id = ctx.session_id }
  if err then error(err, 0) end
  if not view or not ctx.block_id then return end
  for _, window in ipairs(view.windows) do
    local found, main, panes = false, nil, {}
    for _, layer in ipairs(window.layers) do
      for _, block in ipairs(layer.blocks) do
        local caller = block.block_id == ctx.block_id
        found = found or caller
        if layer.kind == "tiled" then
          if block.label and block.label:sub(1, 5) == "milo:" then
            panes[block.label] = block.block_id
          elseif caller or not main then
            main = block.block_id
          end
        end
      end
    end
    if found then return panes, main, window end
  end
end

local function pane(name, cwd)
  return rex.layout.block{
    flavor = terminal .. ".shell",
    label = labels[name],
    options = { command = { milo, name }, cwd = cwd },
  }
end

-- The split never takes the keyboard from the pane the action came from.
local function split(ctx, anchor, direction, side, ratio, layout)
  local _, err = rex.session.new_split{
    session_id = ctx.session_id, anchor_id = anchor, direction = direction,
    side = side, ratio = ratio, layout = layout,
  }
  if err then error(err, 0) end
end

local function arrange(ctx, wanted, shell)
  local present, main
  if shell then
    -- The listener already checked that this tab has only its main shell.
    present, main = {}, ctx.block_id
  else
    present, main = tab_panes(ctx)
  end
  if not main then return end
  local outline = wanted.outline and not present[labels.outline]
  local history = wanted.history and not present[labels.history]
  local usage = wanted.usage and not present[labels.usage]
  if not (outline or history or usage) then return end

  local process = shell or rex.call(terminal .. ".process", { session_id = ctx.session_id, block_id = main })
  local cwd = process and (process.foreground and process.foreground.cwd or process.child and process.child.cwd)
  if outline and history then
    local column = rex.layout.vertical(outline_share, pane("outline", cwd), pane("history", cwd))
    split(ctx, main, "horizontal", "before", column_width, column)
  elseif outline and present[labels.history] then
    split(ctx, present[labels.history], "vertical", "before", outline_share, pane("outline", cwd))
  elseif history and present[labels.outline] then
    split(ctx, present[labels.outline], "vertical", "after", outline_share, pane("history", cwd))
  elseif outline or history then
    local name = outline and "outline" or "history"
    split(ctx, main, "horizontal", "before", column_width, pane(name, cwd))
  end

  -- Read the main pane's height after placing the sidebar.
  if usage then
    local size = rex.call(terminal .. ".size", { session_id = ctx.session_id, block_id = main })
    if size and size.rows and size.rows > usage_rows + 1 then
      split(ctx, main, "vertical", "before", usage_rows / size.rows, pane("usage", cwd))
    end
  end
end

-- The listener uses the same layout, reading the latest choices for each tab.
if ... == "listener" then
  if arrange_new_tabs then return function(ctx, shell) arrange(ctx, chosen, shell) end end
  return nil
end

rex.action{
  name = "milo_layout", title = "Milo: Arrange This Tab",
  run = function(ctx) arrange(ctx, chosen) end,
}
rex.action{
  name = "milo_outline", title = "Milo: Add the Outline",
  run = function(ctx) arrange(ctx, { outline = true }) end,
}
rex.action{
  name = "milo_history", title = "Milo: Add the History",
  run = function(ctx) arrange(ctx, { history = true }) end,
}
rex.action{
  name = "milo_usage", title = "Milo: Add the Usage Strip",
  run = function(ctx) arrange(ctx, { usage = true }) end,
}
rex.action{
  name = "milo_close", title = "Milo: Close Panes in This Tab",
  run = function(ctx)
    local _, _, window = tab_panes(ctx)
    if not window then return end
    for _, layer in ipairs(window.layers) do
      for _, block in ipairs(layer.blocks) do
        local label = block.label
        if label == labels.outline or label == labels.history or label == labels.usage then
          rex.call("block.close", { session_id = ctx.session_id, block_id = block.block_id, keep_last = true })
        end
      end
    end
  end,
}
