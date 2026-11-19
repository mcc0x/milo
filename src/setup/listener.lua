local terminal = "com.superlogical.terminal"

-- A command, a floating pane, or a tab with its own layout is left alone.
local function single_pane(session_id, window_id)
  local view = rex.session.view{ session_id = session_id }
  if not view then return end
  for _, window in ipairs(view.windows or {}) do
    if window.window_id == window_id and not window.zoomed_block_id and #window.layers == 1 then
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

local function arrange_shell(session_id, window_id, retries)
  local block = single_pane(session_id, window_id)
  if not block then return end
  local process = rex.call(terminal .. ".process", { session_id = session_id, block_id = block })
  if not process or not process.foreground then
    -- Starting a shell should not hold up events for other tabs.
    if retries > 0 then
      rex.after(0.05, function() arrange_shell(session_id, window_id, retries - 1) end)
    end
    return
  end
  -- A login shell gives itself a name such as "-zsh"; a command does not.
  local name = process.foreground.argv0 or ""
  if name:sub(1, 1) ~= "-" then return end
  local load_layout = loadfile(rex_config)
  if not load_layout then return end
  local ok, arrange = pcall(load_layout, "listener")
  if ok and arrange then
    pcall(arrange, { session_id = session_id, block_id = block }, process, function(cwd)
      -- Looking up the profile can yield. A user split or a command started
      -- in the meantime must still win over automatic arrangement.
      if single_pane(session_id, window_id) ~= block then return false end
      local current = rex.call(terminal .. ".process", { session_id = session_id, block_id = block })
      local foreground = current and current.foreground
      return foreground and foreground.argv0 and foreground.argv0:sub(1, 1) == "-" and foreground.cwd == cwd
    end)
  end
end

rex.on("window_created", function(ctx, event)
  arrange_shell(ctx.session_id, event.window_id, 9)
end)

-- A new session comes with its first tab, which is not announced on its own.
rex.on("session_created", function(_, event)
  local session_id = event.session and event.session.session_id
  local view = session_id and rex.session.view{ session_id = session_id }
  if not view then return end
  for _, window in ipairs(view.windows) do
    arrange_shell(session_id, window.window_id, 9)
  end
end)
