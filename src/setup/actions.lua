local terminal = "com.superlogical.terminal"
local roles = { "outline", "history", "usage" }
local labels, trial_labels, role_by_label = {}, {}, {}
for _, role in ipairs(roles) do
  local label, trial_label = "milo:" .. role, "milo:trial:" .. role
  labels[role], trial_labels[role] = label, trial_label
  role_by_label[label], role_by_label[trial_label] = role, role
end
local role_by_letter = { o = "outline", h = "history", u = "usage" }
local peek_label = "milo:peek:outline"
-- The server may not have inherited the XDG settings of the shell that
-- installed this file. Read and write the same profiles as that installation.
local source = debug and debug.getinfo and debug.getinfo(1, "S").source or ""
local config_home = source:match("^@(/.*)/milo/rex%.lua$")
if source == "@/milo/rex.lua" then config_home = "/" end

local function profile_exec(argv, timeout)
  local options = { timeout = timeout }
  if config_home then options.env = { XDG_CONFIG_HOME = config_home } end
  return rex.exec(argv, options)
end
local errors = {
  mixed = "Milo panes must share one side rail, separate from other panes.",
  no_rail = "Add Milo panes in a side rail before remembering this layout.",
  no_shell = "Focus the shell Milo should use, then try again.",
  no_tab = "Could not find this Rex tab.",
  zoomed = "Unzoom the tab first.",
  trial = "Finish or undo the Milo setup trial in this tab first.",
}

local function raise(why)
  error(errors[why] or why, 0)
end

local function view(ctx)
  local result, err = rex.session.view{ session_id = ctx.session_id }
  if err then raise(err) end
  return result
end

local function find_block(window, id)
  for _, layer in ipairs(window.layers or {}) do
    for _, block in ipairs(layer.blocks or {}) do
      if block.block_id == id then return block, layer end
    end
  end
end

local function tab(ctx)
  local session = view(ctx)
  for _, window in ipairs(session and session.windows or {}) do
    if ctx.block_id and find_block(window, ctx.block_id) then return window, session end
  end
end

local function tiled(window)
  for _, layer in ipairs(window.layers or {}) do
    if layer.kind == "tiled" then return layer end
  end
end

local function terminal_call(ctx, block, method)
  return rex.call(terminal .. "." .. method, { session_id = ctx.session_id, block_id = block })
end

local function login_shell(process)
  return process and process.argv0 and process.argv0:sub(1, 1) == "-"
end

local function shell_cwd(process)
  if not process then return end
  -- A foreground editor can be in a different directory from its shell.
  for i = #(process.ancestors or {}), 1, -1 do
    local parent = process.ancestors[i]
    if login_shell(parent) and parent.cwd then return parent.cwd end
  end
  for _, kind in ipairs{ "foreground", "child" } do
    local shell = process[kind]
    if login_shell(shell) and shell.cwd then return shell.cwd end
  end
end

local function anchor(ctx, window, allow_pin)
  local layer = tiled(window)
  local caller = find_block(window, ctx.block_id)
  local function candidate(block)
    return block and block.creator_name == terminal and (block.label or ""):sub(1, 5) ~= "milo:"
  end
  local caller_process
  if candidate(caller) then
    caller_process = terminal_call(ctx, caller.block_id, "process")
    local cwd = shell_cwd(caller_process)
    if cwd then return caller.block_id, cwd, caller.block_id end
  end
  local selected, cwd
  for _, block in ipairs(layer and layer.blocks or {}) do
    if block ~= caller and candidate(block) then
      local path = shell_cwd(terminal_call(ctx, block.block_id, "process"))
      if path then
        -- Never choose between two possible shells when invoked from a rail pane.
        if selected then return end
        selected, cwd = block.block_id, path
      end
    end
  end
  if selected then return selected, cwd, selected end
  if allow_pin and caller_process then
    local path = caller_process.child and caller_process.child.cwd
      or caller_process.foreground and caller_process.foreground.cwd
    if path then return caller.block_id, path end
  end
end

local function node_id(node)
  return node and (node.block_id or node.split and node.split.split_id)
end

local function fraction(number)
  return number and number > 0 and number < 1
end

-- Accept exactly one rail at an outer edge. The other side may be any work
-- area, but the rail must contain only Milo roles in a vertical stack.
local function inspect_rail(layer)
  if not layer then return nil, "no_tab" end
  local found, total = {}, 0
  for _, block in ipairs(layer.blocks or {}) do
    local role = role_by_label[block.label]
    if role then
      if block.label == trial_labels[role] then return nil, "trial" end
      if block.creator_name ~= terminal or found[role] then return nil, "mixed" end
      found[role], total = block.block_id, total + 1
    end
  end
  if total == 0 then return nil end
  local root = layer.layout and layer.layout.split
  if not root or root.direction ~= "horizontal" or not fraction(root.ratio) then return nil, "mixed" end
  local ids = {}
  for role, block in pairs(found) do ids[block] = role end
  local function column(node, share, order, shares)
    local role = node and ids[node.block_id]
    if role then
      if shares[role] then return false end
      order[#order + 1], shares[role] = role, share
      return true
    end
    local split = node and node.split
    return split and split.direction == "vertical" and fraction(split.ratio)
      and column(split.before, share * split.ratio, order, shares)
      and column(split.after, share * (1 - split.ratio), order, shares)
  end
  for _, side in ipairs{ "l", "r" } do
    local order, shares = {}, {}
    local node = side == "l" and root.before or root.after
    if column(node, 1, order, shares) and #order == total then
      return { side = side, width = side == "l" and root.ratio or 1 - root.ratio,
        order = order, shares = shares, present = found }
    end
  end
  return nil, "mixed"
end

local function rail_code(rail)
  local order, enabled, shares = {}, {}, {}
  for _, role in ipairs(rail.order) do order[#order + 1] = role:sub(1, 1) end
  for _, role in ipairs(roles) do
    local share = rail.shares[role]
    if not share then order[#order + 1] = role:sub(1, 1) end
    enabled[#enabled + 1] = share and "1" or "0"
    shares[#shares + 1] = share and math.max(1, math.floor(share * 1000000 + 0.5)) or 0
  end
  local width = math.max(1, math.min(999999, math.floor(rail.width * 1000000 + 0.5)))
  return table.concat({ "v1", rail.side, string.format("0.%06d", width), outline_share, usage_rows,
    table.concat(order), table.concat(enabled), table.concat(shares, " ") }, " ")
end

local function profile(code)
  local values = {}
  for value in code:gmatch("%S+") do values[#values + 1] = value end
  if (#values ~= 7 and #values ~= 10) or values[1] ~= "v1" then return end
  local side, width, outline, rows = values[2], tonumber(values[3]), tonumber(values[4]), tonumber(values[5])
  if (side ~= "l" and side ~= "r") or not fraction(width) or not fraction(outline)
    or not rows or rows < 1 or not values[6]:match("^[ohu][ohu][ohu]$") or not values[7]:match("^[01][01][01]$") then
    return
  end
  local order, wanted, seen, shares = {}, {}, {}, nil
  if #values == 10 then shares = {} end
  for i, role in ipairs(roles) do
    local position = role_by_letter[values[6]:sub(i, i)]
    if seen[position] then return end
    order[i], seen[position], wanted[role] = position, true, values[7]:sub(i, i) == "1"
    if shares then
      local share = tonumber(values[7 + i])
      if not share or share < 0 or share > 1000000 then return end
      shares[role] = share / 1000000
    end
  end
  return { side = side, width = width, outline = outline, rows = rows, order = order, wanted = wanted, shares = shares }
end

local function effective(cwd)
  local result = profile_exec({ milo, "layout-profile", "read", cwd or "" }, 2)
  if not result or not result.ok then raise("Could not read the saved Milo layout.") end
  if result.stdout:match("%S") then
    local saved = profile(result.stdout)
    if not saved then raise("Could not read the saved Milo layout.") end
    return saved
  end
  return { side = "l", width = column_width, outline = outline_share, rows = usage_rows, order = roles, wanted = chosen }
end

local function pane(role, cwd, shell, origin, trial)
  local command = { milo, role }
  if origin then
    command = { milo, role, "--peek-origin", origin }
  elseif shell and role ~= "usage" then
    command = { milo, role, "--shell", shell }
  end
  return rex.layout.block{
    flavor = terminal .. ".shell", label = origin and peek_label or trial and trial_labels[role] or labels[role],
    options = { command = command, cwd = cwd },
  }
end

local function weights(selected, rows)
  local wanted, shares = selected.wanted, {}
  local paired = wanted.outline and wanted.history
  local policy = selected.shares
  if wanted.usage then
    -- A lone pane is the whole rail; in a stack Usage is a strip sized in terminal rows.
    -- Match setup's 35-row estimate if no terminal height is reported.
    shares.usage = (wanted.outline or wanted.history) and math.min(selected.rows / (rows or 35), 0.4) or 1
  end
  local rest = 1 - (shares.usage or 0)
  if wanted.outline then shares.outline = rest * (paired and selected.outline or 1) end
  if wanted.history then shares.history = rest * (paired and (1 - selected.outline) or 1) end
  if policy then
    for role in pairs(shares) do
      if policy[role] > 0 then shares[role] = policy[role] end
    end
  end
  return shares
end

local function column(selected, cwd, shell, rows, trial)
  local ordered, shares, remaining = {}, weights(selected, rows), 0
  for _, role in ipairs(selected.order) do
    if shares[role] then ordered[#ordered + 1], remaining = role, remaining + shares[role] end
  end
  local function build(i, total)
    local role = ordered[i]
    local block = pane(role, cwd, shell, nil, trial)
    if i == 1 then return block end
    local rest = total - shares[role]
    return rex.layout.vertical(rest / total, build(i - 1, rest), block)
  end
  if #ordered > 0 then return build(#ordered, remaining), ordered end
end

local function cells(ctx, block, rect)
  local size = terminal_call(ctx, block, "size")
  if size and size.rows and size.rows > 0 then
    local cols, rows = size.columns or size.cols, size.rows
    if rect and rect[3] > 0 and rect[4] > 0 then
      cols, rows = cols and math.floor(cols / rect[3] + 0.5), math.floor(rows / rect[4] + 0.5)
    end
    return cols, rows, size.owner
  end
end

local function rect_for(node, block)
  if not node then return end
  if node.block_id == block then return node.rect end
  local split = node.split
  if split then return rect_for(split.before, block) or rect_for(split.after, block) end
end

local function tab_cells(ctx, layer, source)
  local preferred = source and rect_for(layer.layout, source)
  if preferred then
    local cols, rows = cells(ctx, source, preferred)
    if rows then return cols, rows end
  end
  for _, block in ipairs(layer.blocks or {}) do
    if block.block_id ~= source and block.creator_name == terminal then
      local rect = rect_for(layer.layout, block.block_id)
      if rect then
        local cols, rows = cells(ctx, block.block_id, rect)
        if rows then return cols, rows end
      end
    end
  end
end

local function split(ctx, anchor_id, direction, before, share, layout)
  return rex.session.new_split{
    session_id = ctx.session_id, anchor_id = anchor_id, direction = direction, focus = false,
    side = before and "before" or "after", ratio = before and share or 1 - share, layout = layout,
  }
end

local function close_block(ctx, block)
  return rex.call("block.close", { session_id = ctx.session_id, block_id = block, keep_last = true })
end

local function make_rail(ctx, selected, cwd, shell, root, rows, trial)
  local rail, ordered = column(selected, cwd, shell, rows, trial)
  if not rail then return {}, {} end
  local made, err = split(ctx, root, "horizontal", selected.side == "l", selected.width, rail)
  return made, ordered, err
end

-- Adding a single pane is additive. Never reinterpret or move an existing rail.
local function fill_rail(ctx, selected, live, cwd, shell, rows)
  local desired = weights(selected, rows)
  local positions = {}
  for i, role in ipairs(selected.order) do positions[role] = i end
  for _, role in ipairs(selected.order) do
    if desired[role] and not live.present[role] then
      local successor, predecessor
      for _, existing in ipairs(live.order) do
        if positions[existing] > positions[role] and not successor then successor = existing end
        if positions[existing] < positions[role] then predecessor = existing end
      end
      local neighbour = successor or predecessor
      local before = successor ~= nil
      local share = desired[role] / (desired[role] + (desired[neighbour] or live.shares[neighbour]))
      local height = live.shares[neighbour]
      if role == "usage" and not (selected.shares and selected.shares.usage > 0) then
        -- Convert the whole-tab height to a share of the actual adjacent pane.
        -- A very short neighbour keeps at least half of its existing space.
        share = math.min(desired[role] / height, 0.5)
      end
      local made, err = split(ctx, live.present[neighbour], "vertical", before, share, pane(role, cwd, shell))
      if not made or not made.block_ids or not made.block_ids[1] then raise(err or "Rex could not add the pane.") end
      live.present[role], live.shares[role] = made.block_ids[1], height * share
      live.shares[neighbour] = height * (1 - share)
      for i, existing in ipairs(live.order) do
        if existing == neighbour then table.insert(live.order, before and i or i + 1, role); break end
      end
    end
  end
end

local function arrange(ctx, one)
  local window = tab(ctx)
  if not window then raise("no_tab") end
  if window.zoomed_block_id then raise("zoomed") end
  local source, cwd, shell = anchor(ctx, window, true)
  if not source then raise("no_shell") end
  local window_id = window.window_id
  local selected = effective(cwd)
  -- Profile reads can yield. Don't edit using an old view or a different shell's profile.
  window = tab(ctx)
  if not window or window.window_id ~= window_id then raise("no_tab") end
  if window.zoomed_block_id then raise("zoomed") end
  local current, current_cwd, current_shell = anchor(ctx, window, true)
  if current ~= source or current_cwd ~= cwd or current_shell ~= shell then raise("The shell changed; try again.") end
  local layer = tiled(window)
  local live, why = inspect_rail(layer)
  if why then raise(why) end
  local _, rows = tab_cells(ctx, layer, source)
  if live then
    if one then
      selected.wanted = { [one] = true }
      for _, role in ipairs(roles) do
        if live.present[role] then selected.wanted[role] = true end
      end
    end
    fill_rail(ctx, selected, live, cwd, shell, rows)
  else
    if one then selected.wanted = { [one] = true } end
    local root = node_id(layer.layout)
    if not root then raise("no_tab") end
    local made, _, err = make_rail(ctx, selected, cwd, shell, root, rows)
    if not made then raise(err or "Rex could not add the rail.") end
  end
end

if ... == "listener" then
  local function topology(node)
    if not node then return end
    if node.block_id then return node.block_id end
    local split = node.split
    if not split or not split.split_id then return end
    local before, after = topology(split.before), topology(split.after)
    if before and after then return table.concat({ split.split_id, split.direction, before, after }, " ") end
  end

  local function snapshot(window)
    if window.zoomed_block_id then return end
    local layer = tiled(window)
    local rail = inspect_rail(layer)
    if not rail then return end
    local tree = topology(layer.layout)
    if not tree or not layer.layer_id then return end
    local key = { layer.layer_id, tree }
    for _, role in ipairs(rail.order) do key[#key + 1] = role .. "=" .. rail.present[role] end
    return { key = table.concat(key, " "), code = rail_code(rail), rail = rail }
  end

  local function directories(session_id, rail)
    local ctx, dirs, seen = { session_id = session_id }, {}, {}
    for _, block in pairs(rail.present) do
      local process = terminal_call(ctx, block, "process")
      local child = process and process.child
      -- A renamed shell or a rail from another installation is not ours.
      if not child or (child.path ~= milo and child.invoked_path ~= milo and child.argv0 ~= milo)
        or not child.cwd then return end
      if not seen[child.cwd] then
        dirs[#dirs + 1], seen[child.cwd] = child.cwd, true
      end
    end
    -- Use the panes' launch worktree, not whichever directory a shell now has open.
    return dirs
  end

  local function save(dirs, before, after)
    if before == after then return end
    local argv = { milo, "layout-profile", "resize", before, after }
    for _, cwd in ipairs(dirs) do argv[#argv + 1] = cwd end
    profile_exec(argv, 3)
  end

  local memory = { snapshot = snapshot, directories = directories, save = save }
  if arrange_new_tabs then
    memory.arrange = function(ctx, process, still_single)
      local cwd = shell_cwd(process)
      if not cwd then return end
      local selected = effective(cwd)
      local wanted = selected.wanted
      local needs_rows = wanted.usage and (wanted.outline or wanted.history)
        and not (selected.shares and selected.shares.usage > 0)
      local function arrange_sized(retries)
        local _, rows, owner = cells(ctx, ctx.block_id)
        if not still_single(cwd) then return end
        if needs_rows and type(owner) ~= "string" and retries > 0 then
          -- The login shell can become ready before its client measures the tab.
          rex.after(0.05, function() arrange_sized(retries - 1) end)
        else
          make_rail(ctx, selected, cwd, ctx.block_id, ctx.block_id, rows)
        end
      end
      arrange_sized(9)
    end
  end
  return memory
end

local function scope_for(args, cwd, resetting)
  if args.scope then
    if args.scope == "global" or args.scope == "worktree" then return args.scope end
    raise("Choose global or worktree scope.")
  end
  local items = {}
  if cwd then
    local git = profile_exec({ milo, "layout-profile", "worktree", cwd }, 2)
    if git and git.ok then
      items[#items + 1] = { id = "worktree", title = resetting and "Use global default here" or "For this Git worktree",
        subtitle = "Other worktrees are unchanged", icon = "folder" }
    end
  end
  items[#items + 1] = { id = "global", title = resetting and "Reset my global default" or "As my global default",
    subtitle = "Where there is no worktree override", icon = "gear" }
  local picked, why = rex.ui.pick{ title = resetting and "Reset Milo layout" or "Remember this Milo layout", items = items }
  if not picked and why ~= "cancelled" then raise("Choose a scope with a connected Rex app, or pass scope explicitly.") end
  return picked
end

local function remember(ctx, args, resetting)
  local window = tab(ctx)
  if not window then raise("no_tab") end
  local window_id = window.window_id
  local shell, cwd = anchor(ctx, window)
  local initial
  if not resetting then
    local rail, why = inspect_rail(tiled(window))
    if not rail then raise(why or "no_rail") end
    initial = rail_code(rail)
  end
  local scope = scope_for(args, cwd, resetting)
  if not scope then return end
  window = tab(ctx)
  if not window or window.window_id ~= window_id then raise("no_tab") end
  if scope == "worktree" then
    local current, path = anchor(ctx, window)
    if not cwd or current ~= shell or path ~= cwd then raise("Focus the same shell in a Git worktree and try again.") end
  end
  local argv = { milo, "layout-profile", resetting and "reset" or "save", scope, cwd or "" }
  if initial then
    local rail, why = inspect_rail(tiled(window))
    if not rail then raise(why or "no_rail") end
    if rail_code(rail) ~= initial then raise("The Milo rail changed; try again.") end
    argv[#argv + 1] = initial
  end
  local result = profile_exec(argv, 3)
  if not result or not result.ok then
    local detail = result and result.stderr and result.stderr:match("[^\r\n]+")
    raise(detail or "Could not save the Milo layout.")
  end
  return resetting and "Milo layout reset." or "Milo layout remembered."
end

local function peek_in(window)
  for _, layer in ipairs(window.layers or {}) do
    if layer.kind == "floating" then
      for _, block in ipairs(layer.blocks or {}) do
        if block.label == peek_label and block.creator_name == terminal then return block, layer end
      end
    end
  end
end

local function restore_focus(ctx, window, session, block, origin)
  if not origin or origin == block or session.active_window_id ~= window.window_id or window.focused_block_id ~= block
    or not find_block(window, origin) then return end
  return rex.session.focus_block{ session_id = ctx.session_id, block_id = origin }
end

local origins = {}
local function peek(ctx, closing)
  local window, session = tab(ctx)
  if not window then raise("no_tab") end
  local block, layer = peek_in(window)
  local record = origins[window.window_id]
  if closing then
    if not block then return end
    restore_focus(ctx, window, session, block.block_id, record and record.block == block.block_id and record.origin)
    local _, err = close_block(ctx, block.block_id)
    if err then raise(err) end
    origins[window.window_id] = nil
  elseif block then
    local _, err = rex.call("session.set_layer_state", { session_id = ctx.session_id, layer_id = layer.layer_id,
      state = "normal" })
    if not err then _, err = rex.session.raise_layer{ session_id = ctx.session_id, layer_id = layer.layer_id } end
    if not err then _, err = rex.session.focus_block{ session_id = ctx.session_id, block_id = block.block_id } end
    if err then raise(err) end
  else
    local _, cwd = anchor(ctx, window, true)
    if not cwd then raise("no_shell") end
    local made, err = rex.session.new_layer{
      session_id = ctx.session_id, window_id = window.window_id, focus = true,
      bounds = {0.12, 0.10, 0.55, 0.80}, layout = pane("outline", cwd, nil, ctx.block_id),
    }
    if not made or not made.block_ids or not made.block_ids[1] then raise(err or "Rex could not open Peek Outline.") end
    origins[window.window_id] = { block = made.block_ids[1], origin = ctx.block_id }
  end
end

-- Setup uses a private script copy of these exact operations before installing anything.
local operation = rex.args and rex.args.milo_operation
if operation then
  local ctx = { session_id = rex.session_id, block_id = rex.block_id }
  local function probe()
    if operation == "undo" or operation == "keep" then
      local session = view(ctx)
      for _, window in ipairs(session and session.windows or {}) do
        if window.window_id == rex.args.window then
          local failed = false
          for _, role in ipairs(roles) do
            if rex.args[role] then
              local block = find_block(window, rex.args[role])
              if block and block.label == trial_labels[role] and block.creator_name == terminal then
                local done
                if operation == "undo" then
                  done = close_block(ctx, block.block_id)
                else
                  done = rex.session.set_block_label{
                    session_id = ctx.session_id, block_id = block.block_id, label = labels[role],
                  }
                end
                failed = failed or not done
              end
            end
          end
          if failed then return "no_undo" end
          break
        end
      end
      return "ok"
    end
    local window, session = tab(ctx)
    if not window then return "no_tab" end
    local layer = tiled(window)
    if operation == "return_peek" then
      local block = peek_in(window)
      if block and block.block_id == ctx.block_id then restore_focus(ctx, window, session, ctx.block_id, rex.args.origin) end
      return "ok"
    end
    if operation == "capture" then
      local rail, why = inspect_rail(layer)
      return rail and "ok " .. rail_code(rail) or why or "no_rail"
    end
    local source, cwd, shell = anchor(ctx, window, operation == "trial")
    if operation == "context" then
      local path = cwd and cwd:gsub(".", function(c) return string.format("%02x", c:byte()) end) or "-"
      local cols, rows
      if layer then cols, rows = tab_cells(ctx, layer, source) end
      return "ok " .. path .. " " .. (cols or 0) .. " " .. (rows or 0)
    end
    if not layer then return "no_tab" end
    if operation == "trial" then
      if window.zoomed_block_id then return "zoomed" end
      if not source then return "no_shell" end
      local selected = profile(trial_profile)
      if not selected then return "no_replay" end
      local live, why = inspect_rail(layer)
      local _, rows = tab_cells(ctx, layer, source)
      local made, ordered, mode
      if live or why == "mixed" or why == "trial" then
        -- Preview over the chosen edge when another rail already exists.
        -- Never move or close existing panes just to show the draft.
        local layout
        layout, ordered = column(selected, cwd, shell, rows, true)
        if not layout then return "no_panes" end
        mode = "floating"
        made = rex.session.new_layer{
          session_id = ctx.session_id, window_id = window.window_id, focus = false, layout = layout,
          bounds = { selected.side == "l" and 0 or 1 - selected.width, 0, selected.width, 1 },
        }
      else
        local root = node_id(layer.layout)
        if not root then return "no_tab" end
        mode = "tiled"
        made, ordered = make_rail(ctx, selected, cwd, shell, root, rows, true)
        if ordered and #ordered == 0 then return "no_panes" end
      end
      if not made or not made.block_ids or #made.block_ids ~= #ordered then return "no_replay" end
      local result = "ok " .. window.window_id .. " " .. mode
      for i, role in ipairs(ordered) do result = result .. " " .. role .. "=" .. made.block_ids[i] end
      return result
    end
    return "no_tab"
  end
  print(probe())
  return
end

rex.action{ name = "milo_layout", title = "Milo: Arrange This Tab", run = function(ctx) arrange(ctx) end }
rex.action{ name = "milo_outline", title = "Milo: Add the Outline", run = function(ctx) arrange(ctx, "outline") end }
rex.action{ name = "milo_history", title = "Milo: Add the History", run = function(ctx) arrange(ctx, "history") end }
rex.action{ name = "milo_usage", title = "Milo: Add the Usage", run = function(ctx) arrange(ctx, "usage") end }
rex.action{ name = "milo_remember_layout", title = "Milo: Remember This Layout…", args = { scope = "string?" },
  run = function(ctx, args) return remember(ctx, args or {}, false) end }
rex.action{ name = "milo_reset_layout", title = "Milo: Reset Saved Layout…", args = { scope = "string?" },
  run = function(ctx, args) return remember(ctx, args or {}, true) end }
rex.action{ name = "milo_peek_outline", title = "Milo: Peek Outline", run = function(ctx) peek(ctx, false) end }
rex.action{ name = "milo_close_peek", title = "Milo: Dismiss Outline Peek", run = function(ctx) peek(ctx, true) end }
rex.action{
  name = "milo_close", title = "Milo: Close Panes in This Tab",
  run = function(ctx)
    local window = tab(ctx)
    if not window then return end
    peek(ctx, true)
    for _, layer in ipairs(window.layers or {}) do
      for _, block in ipairs(layer.blocks or {}) do
        if role_by_label[block.label] and block.creator_name == terminal then close_block(ctx, block.block_id) end
      end
    end
  end,
}
