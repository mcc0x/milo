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
  local made, err = rex.session.new_split{
    session_id = ctx.session_id, anchor_id = anchor, direction = direction,
    side = side, ratio = ratio, layout = layout,
  }
  if err then error(err, 0) end
  return made and made.block_ids and made.block_ids[1]
end

-- The left column holds the outline, the history and the usage, top to bottom.
local function arrange(ctx, wanted, shell)
  local present, main
  if shell then
    -- The listener already checked that this tab has only its main shell.
    present, main = {}, ctx.block_id
  else
    present, main = tab_panes(ctx)
  end
  if not main then return end
  local above, below, usage = present[labels.outline], present[labels.history], present[labels.usage]
  local add = {
    outline = wanted.outline and not above,
    history = wanted.history and not below,
    usage = wanted.usage and not usage,
  }
  if not (add.outline or add.history or add.usage) then return end

  local process = shell or rex.call(terminal .. ".process", { session_id = ctx.session_id, block_id = main })
  local cwd = process and (process.foreground and process.foreground.cwd or process.child and process.child.cwd)
  -- The share of a pane's height left to it once the usage has its rows below.
  -- A pane no app has shown yet reports a size that is not its own.
  local function above_usage(block)
    local size = rex.call(terminal .. ".size", { session_id = ctx.session_id, block_id = block })
    local rows = size and size.owner and size.rows or 0
    return 1 - (rows > 0 and math.min(usage_rows / rows, 0.4) or 0.15)
  end

  if not (above or below or usage) then
    local column
    if add.outline and add.history then
      column = rex.layout.vertical(outline_share, pane("outline", cwd), pane("history", cwd))
    elseif add.outline or add.history then
      column = pane(add.outline and "outline" or "history", cwd)
    end
    if add.usage then
      -- The column is as tall as the main pane, which is the whole tab here.
      local strip = pane("usage", cwd)
      column = column and rex.layout.vertical(above_usage(main), column, strip) or strip
    end
    split(ctx, main, "horizontal", "before", column_width, column)
    return
  end

  -- The column is there already, so each missing pane goes beside a neighbour.
  if add.outline then
    if below then
      above = split(ctx, below, "vertical", "before", outline_share, pane("outline", cwd))
    else
      above = split(ctx, usage, "vertical", "before", above_usage(usage), pane("outline", cwd))
    end
  end
  if add.history then
    if above then
      below = split(ctx, above, "vertical", "after", outline_share, pane("history", cwd))
    else
      below = split(ctx, usage, "vertical", "before", above_usage(usage), pane("history", cwd))
    end
  end
  if add.usage then
    local neighbour = below or above
    split(ctx, neighbour, "vertical", "after", above_usage(neighbour), pane("usage", cwd))
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
  name = "milo_usage", title = "Milo: Add the Usage",
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
