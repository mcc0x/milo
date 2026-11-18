local terminal = "com.superlogical.terminal"

-- A command, a floating pane, or a tab with its own layout is left alone.
local function single_pane(session_id, window_id)
  local view = rex.session.view{ session_id = session_id }
  if not view then return end
  for _, window in ipairs(view.windows) do
    if window.window_id == window_id and #window.layers == 1 then
      local layer = window.layers[1]
      if layer.kind == "tiled" and #layer.blocks == 1 then
        local block = layer.blocks[1]
        if not block.label or block.label:sub(1, 5) ~= "milo:" then
          return block.block_id
        end
      end
    end
  end
end

-- A tab opened as a plain shell runs a login shell, whose own name for
-- itself starts with a dash ("-zsh"). One opened to run a command does not.
local function is_login_shell(process)
  local name = process.foreground.argv0 or ""
  return name:sub(1, 1) == "-"
end

rex.on("window_created", function(ctx, event)
  -- For a moment a new pane has nothing running in it yet: look again shortly.
  for attempt = 1, 10 do
    local block = single_pane(ctx.session_id, event.window_id)
    if not block then return end
    local process = rex.block.call(terminal, "process", { block_id = block })
    if process and process.foreground then
      if not is_login_shell(process) then return end
      local load_layout = loadfile(rex_config)
      if not load_layout then return end
      local ok, arrange = pcall(load_layout, "listener")
      if ok and arrange then
        pcall(arrange, { session_id = ctx.session_id, block_id = block })
      end
      return
    end
    if attempt < 10 then rex.sleep(0.05) end
  end
end)
