local terminal = "com.superlogical.terminal"
local roles = { "outline", "history", "usage" }
local labels, trial_labels, role_by_label = {}, {}, {}
for _, role in ipairs(roles) do
  local label, trial_label = "milo:" .. role, "milo:trial:" .. role
  labels[role], trial_labels[role] = label, trial_label
  role_by_label[label], role_by_label[trial_label] = role, role
end
local role_by_letter = { o = "outline", h = "history", u = "usage" }
local shell_flavor = terminal .. ".shell"
local shell_names = { bash = true, csh = true, dash = true, fish = true, ksh = true,
  sh = true, tcsh = true, zsh = true }
local peek_label = "milo:peek:outline"
-- The server may not have inherited the XDG settings of the shell that
-- installed this file. Read and write the same profiles as that installation.
local source = debug and debug.getinfo and debug.getinfo(1, "S").source or ""
local config_home = source:match("^@(/.*)/milo/rex%.lua$")
if source == "@/milo/rex.lua" then config_home = "/" end

local function profile_exec(argv, timeout, stdin)
  local options = { timeout = timeout, stdin = stdin }
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
  no_terminal = "Focus the terminal whose output names the file, then try again.",
  milo_pane = "This is a Milo pane. Focus the terminal whose output names the file, then try again.",
  other_host = "This pane's output may name files on another computer, so Milo opened nothing.",
  no_folder = "Milo could not tell which folder this pane is working in, so it opened nothing.",
  no_output = "Milo could not read this pane's recent output.",
  no_files = "No project files found in this pane's recent output.",
  outside_only = "This pane's recent output names only files outside its worktree, so Milo opened nothing.",
  not_offered = "That file is not in this pane's recent output.",
  no_picker = "Choose a file in a connected Rex app, or pass choice.",
  no_reader = "Rex could not open the reader.",
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
  local found, blocks, total = {}, {}, 0
  for _, block in ipairs(layer.blocks or {}) do
    local role = role_by_label[block.label]
    if role then
      if block.label == trial_labels[role] then return nil, "trial" end
      if block.creator_name ~= terminal or found[role] then return nil, "mixed" end
      found[role], blocks[role], total = block.block_id, block, total + 1
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
        order = order, shares = shares, present = found, blocks = blocks,
        work = node_id(side == "l" and root.after or root.before) }
    end
  end
  return nil, "mixed"
end

local function restored_shell(process)
  local child, front = process.child, process.foreground
  if not (type(child) == "table" and type(front) == "table"
    and type(child.pid) == "number" and child.pid > 0 and type(front.pid) == "number" and front.pid > 0
    and type(front.user_id) == "number" and shell_names[front.name]
    and type(front.path) == "string" and front.path:sub(1, 1) == "/"
    and type(front.cwd) == "string" and front.cwd:sub(1, 1) == "/"
    and type(front.argv0) == "string" and login_shell(front)) then return end
  local invoked = type(front.invoked_path) == "string" and front.invoked_path:match("([^/]+)$")
  if front.argv0 ~= "-" .. front.name and (not invoked or front.argv0 ~= "-" .. invoked) then return end
  local ancestors = process.ancestors or {}
  -- macOS starts the shell under login. On other Unix systems it can
  -- be the PTY's direct child. A nested shell is not the original shell.
  if child.pid == front.pid and #ancestors == 0 and child.path == front.path and child.user_id == front.user_id then
    return { pid = front.pid, path = front.path, cwd = front.cwd, user = front.user_id }
  elseif child.path == "/usr/bin/login" and #ancestors == 1
    and ancestors[1].pid == child.pid and ancestors[1].path == child.path and front.pid ~= child.pid then
    return { pid = front.pid, path = front.path, cwd = front.cwd, user = front.user_id, login = child.pid }
  end
end

local function rail_processes(ctx, rail)
  local shells, running = {}, 0
  for _, role in ipairs(rail.order) do
    local block = rail.blocks[role]
    if block.flavor ~= shell_flavor then return "unverified" end
    local process = terminal_call(ctx, block.block_id, "process")
    if not process or type(process.last_exit) == "table" then return "unverified" end
    local child = process.child
    if child and (child.path == milo or child.invoked_path == milo) then
      running = running + 1
    else
      shells[role] = restored_shell(process)
      if not shells[role] then return "unverified" end
    end
  end
  if running == #rail.order then return "running" end
  if running == 0 then return "restored", shells end
  return "unverified"
end

-- Rex's foreground reply doesn't reveal background jobs. Check that the
-- shell has no children and nothing else still holds its controlling tty.
-- Never use local process IDs for blocks from another host or account.
local function quiet_rail(rail, shells, host)
  if type(host) ~= "string" or host == "" then return false end
  for _, role in ipairs(rail.order) do
    if rail.blocks[role].host_id ~= host then return false end
  end
  local result = rex.exec({ "/bin/ps", "-A", "-o", "pid=,ppid=,tty=" }, { timeout = 2, env = { LC_ALL = "C" } })
  if not result or not result.ok or type(result.stdout) ~= "string" then return false end
  local processes = {}
  for line in result.stdout:gmatch("[^\r\n]+") do
    local pid, parent, tty = line:match("^%s*(%d+)%s+(%d+)%s+(%S+)%s*$")
    if not pid then return false end
    processes[tonumber(pid)] = { parent = tonumber(parent), tty = tty }
  end
  for _, shell in pairs(shells) do
    local process = processes[shell.pid]
    if not process or process.tty:match("^%?+$") or process.tty == "-"
      or (shell.login and (not processes[shell.login] or process.parent ~= shell.login)) then return false end
    for pid, other in pairs(processes) do
      if pid ~= shell.pid and pid ~= shell.login
        and (other.parent == shell.pid or (shell.login and other.parent == shell.login)
          or other.tty == process.tty) then return false end
    end
  end
  return true
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
    if shell then command[5], command[6] = "--shell", shell end
  elseif shell and role ~= "usage" then
    command = { milo, role, "--shell", shell }
  end
  return rex.layout.block{
    flavor = shell_flavor, label = origin and peek_label or trial and trial_labels[role] or labels[role],
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

local function tab_cells(ctx, layer, source, measured)
  local preferred = source and rect_for(layer.layout, source)
  if preferred then
    local cols, rows, owner = cells(ctx, source, preferred)
    if rows and (not measured or type(owner) == "string") then return cols, rows end
  end
  for _, block in ipairs(layer.blocks or {}) do
    if block.block_id ~= source and block.creator_name == terminal then
      local rect = rect_for(layer.layout, block.block_id)
      if rect then
        local cols, rows, owner = cells(ctx, block.block_id, rect)
        if rows and (not measured or type(owner) == "string") then return cols, rows end
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
      -- A pane that was closed left its room to one of its neighbours. Take
      -- the room back from the one that holds more than its own share.
      if successor and predecessor then
        local function spare(other) return live.shares[other] - (desired[other] or live.shares[other]) end
        if spare(predecessor) > spare(successor) then neighbour = predecessor end
      end
      local before = neighbour == successor
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

local unverified_rail = "Milo could not verify what is running in this sidebar. Check its panes and close only shells you no longer need, then run Milo: Arrange This Tab again."

local function rebuild_rail(ctx, window_id, live, shells, source, cwd, shell, selected, layer)
  local function require_panes(layout)
    if not next(layout.wanted) then raise("Choose panes in Milo Settings before rebuilding the sidebar.") end
  end
  require_panes(selected)
  if not live.work or not rect_for(layer.layout, source) then raise("Focus a shell outside the sidebar, then try again.") end
  local server = rex.call("server.status")
  local host = server and server.host_id
  local function require_quiet(rail, processes)
    if not quiet_rail(rail, processes, host) then
      raise("Milo could not verify that the restored shells are idle. Finish any jobs in them, then try again. If needed, close only the shells you no longer need before arranging.")
    end
  end
  require_quiet(live, shells)
  local count = #live.order
  local answer, why = rex.ui.pick{ title = "Rebuild Milo sidebar?", items = {
    { id = "keep", title = "Keep shells", subtitle = "Choose this if there is anything in them you want to keep." },
    { id = "rebuild", title = "Rebuild sidebar", icon = "terminal",
      subtitle = "Closes " .. count .. " restored " .. (count == 1 and "shell" or "shells") .. " and discards their input and scrollback." },
  } }
  if not answer and why ~= "cancelled" then
    raise("Run Milo: Arrange This Tab in a connected Rex app to confirm the rebuild. Without an app, check the restored shells and close only those you no longer need before arranging.")
  end
  if answer ~= "rebuild" then return end

  selected = effective(cwd)
  require_panes(selected)
  local changed = "The tab or a restored shell changed. Check the sidebar, then run Milo: Arrange This Tab again."
  local window = tab(ctx)
  if not window or window.window_id ~= window_id or window.zoomed_block_id then raise(changed) end
  local current, path, follows = anchor(ctx, window, true)
  if current ~= source or path ~= cwd or follows ~= shell then raise(changed) end
  local _, rows = tab_cells(ctx, tiled(window), source)

  -- Recheck the whole rail after confirmation, then recheck again before
  -- each close: another client can change panes while a Rex call is pending.
  local work_ctx = { session_id = ctx.session_id, block_id = source }
  for _, role in ipairs(live.order) do
    local current, session = tab(work_ctx)
    if not current or current.window_id ~= window_id or current.zoomed_block_id then raise(changed) end
    local rail = inspect_rail(tiled(current))
    if not rail or rail.work ~= live.work or rail.side ~= live.side then raise(changed) end
    for _, name in ipairs(roles) do
      if rail.present[name] ~= live.present[name] then raise(changed) end
    end
    local verdict, processes = rail_processes(ctx, rail)
    if verdict ~= "restored" then raise(changed) end
    for name, info in pairs(processes) do
      local before = shells[name]
      if info.pid ~= before.pid or info.login ~= before.login or info.path ~= before.path
        or info.cwd ~= before.cwd or info.user ~= before.user then raise(changed) end
    end
    require_quiet(rail, processes)
    if type(session.revision) ~= "number" or view(ctx).revision ~= session.revision then raise(changed) end
    local done, err = close_block(ctx, live.present[role])
    if not done then raise(err or "Rex could not close a restored shell.") end
    live.present[role] = nil
  end
  window = tab(work_ctx)
  layer = window and tiled(window)
  if not layer or window.window_id ~= window_id or window.zoomed_block_id
    or node_id(layer.layout) ~= live.work then raise(changed) end
  local made, _, err = make_rail(ctx, selected, cwd, shell, live.work, rows)
  if not made then raise(err or "Rex could not rebuild the sidebar. Run Milo: Arrange This Tab to try again.") end
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
  if live then
    local verdict, shells = rail_processes(ctx, live)
    if verdict == "restored" then
      if one then raise("Run Milo: Arrange This Tab to rebuild the restored sidebar.") end
      return rebuild_rail(ctx, window_id, live, shells, source, cwd, shell, selected, layer)
    elseif verdict ~= "running" then
      raise(unverified_rail)
    end
  end
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

  -- Tab titles come from process names and directories only. A terminal's
  -- title, screen and status text can hold a prompt, so none of them is read.
  local agents = { claude = true, codex = true, omp = true }
  local function agent_of(process)
    for _, field in ipairs{ "name", "argv0", "invoked_path" } do
      local value = process and process[field]
      local name = type(value) == "string" and value:match("[^/]+$")
      if name and agents[name] then return name end
    end
  end

  local function pane_facts(process)
    if not process then return {} end
    local foreground, child = process.foreground, process.child
    local agent = agent_of(foreground)
    for _, parent in ipairs(process.ancestors or {}) do agent = agent or agent_of(parent) end
    local cwd = shell_cwd(process) or foreground and foreground.cwd or child and child.cwd
    if type(cwd) ~= "string" or cwd:sub(1, 1) ~= "/" then cwd = nil end
    return { cwd = cwd, agent = agent or agent_of(child) }
  end

  local function title_exec(argv)
    local result = profile_exec(argv, 3)
    return result and result.ok and result.stdout
  end

  -- What a file holds now: its bytes, "" for an empty one or a folder, and
  -- false for one that is not there.
  local function reading(path, readings)
    local text = readings[path]
    if text == nil then
      local file = io.open(path, "r")
      if file then
        -- Far more than Milo accepts in a preference file, so any two it would read differ here.
        text = file:read(1048576) or ""
        file:close()
      else
        text = false
      end
      readings[path] = text
    end
    return text
  end

  -- Milo's answers by directory, so that a change it has already answered
  -- for starts neither Milo nor Git. An answer names its grounds, the files
  -- it depends on, and stands while they read the same. One without grounds
  -- is not kept.
  local answers, kept = {}, 0
  local function stands(answer, readings)
    for path, text in pairs(answer.grounds) do
      if reading(path, readings) ~= text then return false end
    end
    return true
  end

  local function same_files(one, other)
    for path in pairs(one) do
      if other[path] == nil then return false end
    end
    for path in pairs(other) do
      if one[path] == nil then return false end
    end
    return true
  end

  local function ask(cwd, agent, readings)
    readings = readings or {}
    local answer = answers[cwd]
    if answer and not stands(answer, readings) then answers[cwd], kept, answer = nil, kept - 1, nil end
    local who = agent or ""
    local known = answer and (answer.untitled and "" or answer.titles[who])
    if known then return known, true end
    local reply = title_exec{ milo, "tab-title", "title", cwd, agent }
    local title, rest = (reply or ""):match("^=([^\r\n]*)(.*)")
    if not title then return end
    local grounds = {}
    for path in rest:gmatch("[^\r\n]+") do grounds[path] = reading(path, readings) end
    if not next(grounds) then return title end
    -- For the same files, keep the earlier readings: they were taken before Milo read them.
    if not (answer and same_files(answer.grounds, grounds)) then
      if answer then kept = kept - 1 end
      -- A listener can outlive many folders. Start again rather than grow without end,
      -- but only once it holds as many folders as Rex's default limits allow tabs:
      -- starting again makes every open tab ask once more.
      if kept >= 4096 then answers, kept = {}, 0 end
      answer = { grounds = grounds, titles = {} }
      answers[cwd], kept = answer, kept + 1
    end
    -- Without a title, titles are off here or this is no project, whoever runs in it.
    if title == "" then answer.untitled = true else answer.titles[who] = title end
    return title, true
  end

  memory.titles = {
    facts = pane_facts,
    -- The label a tab should show, where "" is Rex's own. Nil if Milo could not say.
    -- A second result says whether the answer has grounds that events can recheck.
    ask = ask,
    -- The titles Milo set and still stands by, by window ID. They outlive this listener.
    owned = function()
      local claims = {}
      for id, title in (title_exec{ milo, "tab-title", "owned" } or ""):gmatch("(window:[%x-]+)\t([^\n]+)") do
        claims[id] = title
      end
      return claims
    end,
    claim = function(window_id, title) title_exec{ milo, "tab-title", "claim", window_id, title } end,
    forget = function(ids)
      local argv = { milo, "tab-title", "forget" }
      for _, id in ipairs(ids) do argv[#argv + 1] = id end
      title_exec(argv)
    end,
  }
  if arrange_new_tabs then
    local function uses_rows(selected)
      local wanted = selected.wanted
      return wanted.usage and (wanted.outline or wanted.history)
        and not (selected.shares and selected.shares.usage > 0)
    end

    local function owned_rows(ctx)
      local size = terminal_call(ctx, ctx.block_id, "size")
      if not size or type(size.owner) ~= "string" or not size.rows or size.rows <= 0 then return end
      -- The block can have an owner before its client has measured the tab.
      for _, request in ipairs(size.desired or {}) do
        if request.client_id == size.owner and request.rows == size.rows then return size.rows, size.owner end
      end
    end

    memory.prepare_arrangement = function(ctx, cwd)
      local selected = effective(cwd)
      local needs_rows = uses_rows(selected)
      return function(still_current)
        if not selected then
          if not owned_rows(ctx) then return false end
          selected = effective(cwd)
          needs_rows = uses_rows(selected)
        end
        local rows, owner
        if needs_rows then rows, owner = owned_rows(ctx) end
        if needs_rows and not rows then
          -- Re-read the profile when a background tab is finally measured.
          selected = nil
          return false
        end
        -- The tab waits when its measuring client changed or its shell moved.
        local current, wait = still_current(owner)
        if wait then return false end
        if current then make_rail(ctx, selected, cwd, ctx.block_id, ctx.block_id, rows) end
        return true
      end
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
    local _, cwd, shell = anchor(ctx, window, true)
    if not cwd then raise("no_shell") end
    local made, err = rex.session.new_layer{
      session_id = ctx.session_id, window_id = window.window_id, focus = true,
      bounds = {0.12, 0.10, 0.55, 0.80}, layout = pane("outline", cwd, shell, ctx.block_id),
    }
    if not made or not made.block_ids or not made.block_ids[1] then raise(err or "Rex could not open Peek Outline.") end
    origins[window.window_id] = { block = made.block_ids[1], origin = ctx.block_id }
  end
end

local function diagnose(ctx, window, layer)
  if window.zoomed_block_id then return "zoomed" end
  local live, why = inspect_rail(layer)
  if not live then return why or "no_rail" end
  local verdict = rail_processes(ctx, live)
  if verdict ~= "running" then return verdict end
  local selected = profile(trial_profile)
  if not selected then return "unavailable" end

  local usage = live.present.usage
  local size = usage and terminal_call(ctx, usage, "size")
  -- Before a client measures the pane, Rex can report a fallback PTY size.
  local rows = size and type(size.owner) == "string" and size.rows and size.rows > 0 and size.rows
  local expected, different
  if selected.wanted.usage and (selected.wanted.outline or selected.wanted.history) then
    local _, tab_rows = tab_cells(ctx, layer, ctx.block_id, true)
    if tab_rows and tab_rows > 0 then
      local share = weights(selected, tab_rows)
      local target = tab_rows * share.usage / (share.usage + (share.outline or 0) + (share.history or 0))
      expected = math.max(1, math.floor(target + 0.5))
      -- A lone Usage pane intentionally fills its sidebar. For a stack, allow
      -- one row of rounding and require the divider to disagree too: fonts can
      -- change the terminal's row count without moving any divider.
      different = rows and live.shares.usage < 1 and math.abs(rows - expected) > 1
        and math.abs(live.shares.usage * tab_rows - target) > 1
    end
  end
  return table.concat({ "ok", live.side, string.format("%.9g", live.width),
    usage and (rows or "-") or "absent", expected or "-", different and "different" or "ok" }, " ")
end

-- Setup uses a private script copy of these exact operations before installing anything.
local operation = rex.args and rex.args.milo_operation
if operation then
  local ctx = { session_id = rex.session_id, block_id = rex.block_id }
  local function probe()
    -- The CLI can resolve labels or suffixes. Doctor must only use its caller.
    if operation == "doctor" and (not rex.args.caller or ctx.block_id ~= rex.args.caller) then return "no_tab" end
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
    if operation == "doctor" then return diagnose(ctx, window, layer) end
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

-- The only place Milo names a session: the one this action was run in, and
-- only when someone runs it. It makes no session and shows none.
local function name_session(ctx)
  local window = tab(ctx)
  if not window then raise("no_tab") end
  local _, cwd = anchor(ctx, window, true)
  if not cwd then raise("no_shell") end
  local reply = profile_exec({ milo, "tab-title", "name", cwd }, 3)
  local label = (reply and reply.ok and reply.stdout or ""):match("^([^\n]+)\n$")
  if not label then
    raise(reply and reply.stderr and reply.stderr:match("[^\r\n]+") or "Could not name this session.")
  end
  local named, err = rex.session.set_label{ session_id = ctx.session_id, label = label }
  if not named then raise(err or "Rex could not rename this session.") end
  return "Named this session " .. label .. "."
end
rex.action{ name = "milo_name_session", title = "Milo: Name Session from This Project", run = name_session }

-- How much of a pane's most recent output is read when someone asks to open a
-- file from it. Nothing is read before that, and nothing is kept afterwards.
local output_bytes = 256 * 1024
-- Programs whose output comes from another computer, where the same path is another file.
local remote_programs = { ssh = true, slogin = true, autossh = true, mosh = true, ["mosh-client"] = true,
  telnet = true, et = true }

local function runs_milo(process)
  local child = process and process.child
  return child and (child.path == milo or child.invoked_path == milo) and child
end

-- The terminal the action was run in, and the folder its program works in.
-- Refuse rather than guess whose files its output names.
local function output_source(ctx)
  local window = tab(ctx)
  if not window then raise("no_tab") end
  local block, layer = find_block(window, ctx.block_id)
  if not block or block.creator_name ~= terminal then raise("no_terminal") end
  local process = terminal_call(ctx, block.block_id, "process")
  if (block.label or ""):sub(1, 5) == "milo:" or runs_milo(process) then raise("milo_pane") end
  local server = rex.call("server.status")
  local host = server and server.host_id
  if type(host) ~= "string" or host == "" or block.host_id ~= host then raise("other_host") end
  local front = process and (process.foreground or process.child)
  if not front then raise("no_folder") end
  if remote_programs[front.name] then raise("other_host") end
  for _, parent in ipairs(process.ancestors or {}) do
    if remote_programs[parent.name] then raise("other_host") end
  end
  if type(front.cwd) ~= "string" or front.cwd:sub(1, 1) ~= "/" then raise("no_folder") end
  return { block = block.block_id, cwd = front.cwd, tiled = layer.kind == "tiled" }
end

-- The end of what the terminal shows and has scrolled, as plain text. Rex
-- returns the start of long output first, so its end is read from the result
-- Rex kept.
local function recent_output(ctx, block)
  local function ask(method, args)
    return rex.call(terminal .. "." .. method, { session_id = ctx.session_id, block_id = block, args = args })
  end
  local reply = ask("format", { format = "text", trim = true, unwrap = true, max_bytes = output_bytes })
  if not reply or type(reply.content) ~= "string" then raise("no_output") end
  if not reply.id then return reply.content end
  local total = tonumber(reply.total)
  if not total then raise("no_output") end
  local offset, parts, size, whole = math.max(0, total - output_bytes), {}, 0, false
  for _ = 1, 64 do
    local part = ask("blob_read", { id = reply.id, offset = offset + size, max_bytes = output_bytes })
    if not part or type(part.data) ~= "string" then raise("no_output") end
    parts[#parts + 1], size, whole = part.data, size + #part.data, part.eof
    if whole or #part.data == 0 then break end
  end
  -- Without its end, the output would be missing what was printed last.
  if not whole then raise("no_output") end
  local text = table.concat(parts)
  -- The read may begin partway through a line.
  if offset > 0 then text = text:match("^[^\n]*\n(.*)$") or "" end
  return text
end

-- The floating terminals of a tab that are not Milo's rails or peeks. Rex does
-- not say on every system what runs in one, so Milo asks each: only a reader
-- listens, and only one opened for the same worktree takes the file.
local function floating_terminals(window)
  local found = {}
  for _, layer in ipairs(window.layers or {}) do
    for _, block in ipairs(layer.kind == "floating" and layer.blocks or {}) do
      if block.creator_name == terminal and (block.label or ""):sub(1, 5) ~= "milo:" then
        found[#found + 1] = { block = block.block_id, layer = layer.layer_id }
      end
    end
  end
  return found
end

local function fields(line)
  local found = {}
  for field in (line .. "\t"):gmatch("([^\t]*)\t") do found[#found + 1] = field end
  return found
end

local function output_reply(reply, fallback)
  if reply and reply.ok and type(reply.stdout) == "string" then return reply.stdout end
  raise(reply and reply.stderr and reply.stderr:match("[^\r\n]+") or fallback)
end

-- Shows one place of the source's worktree: in a reader the tab already has
-- for that worktree, which keeps its drafts, or else in a new one. No pane is
-- closed or replaced, and the source terminal is not written to.
local function show_place(ctx, source, place)
  -- The context names the pane the action was run in, wherever the focus is by now.
  local window = tab(ctx)
  if not window then raise("no_tab") end
  local readers = floating_terminals(window)
  local argv = { milo, "output-files", "open", source.cwd, place }
  for _, reader in ipairs(readers) do argv[#argv + 1] = reader.block end
  local answer = fields(output_reply(profile_exec(argv, 10), "no_reader"):match("^([^\n]*)\n$") or "")
  local label = answer[#answer] ~= "" and answer[#answer] or nil
  if answer[1] == "shown" and #answer == 3 then
    for _, reader in ipairs(readers) do
      if reader.block == answer[2] then
        rex.call("session.set_layer_state", { session_id = ctx.session_id, layer_id = reader.layer, state = "normal" })
        rex.session.raise_layer{ session_id = ctx.session_id, layer_id = reader.layer }
        local shown, err = rex.session.focus_block{ session_id = ctx.session_id, block_id = reader.block }
        if not shown then raise(err or "no_reader") end
        if label then
          rex.session.set_block_label{ session_id = ctx.session_id, block_id = reader.block, label = label }
        end
        return
      end
    end
  elseif answer[1] == "view" and #answer == 5 then
    local command = { milo, "view", answer[3] }
    if answer[4] ~= "-" then command[4], command[5] = "--line", answer[4] end
    if source.tiled then
      -- A Markdown preview opened from this reader goes beside the source terminal.
      local origin = table.concat({ ctx.session_id, window.window_id, "shell", source.block }, " ")
      table.insert(command, 1, "MILO_PREVIEW_SOURCE=" .. origin)
      table.insert(command, 1, "/usr/bin/env")
    end
    local made, err = rex.session.new_layer{
      session_id = ctx.session_id, window_id = window.window_id, focus = true, bounds = {0.12, 0.08, 0.76, 0.84},
      layout = rex.layout.block{ flavor = shell_flavor, label = label,
        options = { command = command, shell = "none", cwd = answer[2] } },
    }
    if not made or not made.block_ids or not made.block_ids[1] then raise(err or "no_reader") end
    return
  end
  raise("no_reader")
end

-- Reads the invoking terminal's recent output once, offers the project files
-- it names, and opens the one chosen. The output is only text to look
-- through: nothing in it is run, opened as an address or kept.
local function open_output(ctx, args)
  local source = output_source(ctx)
  local text = recent_output(ctx, source.block)
  local listed = output_reply(profile_exec({ milo, "output-files", "list", source.cwd }, 10, text), "no_output")
  local head, rows = listed:match("^([^\n]*)\n(.*)$")
  local outside = (head or ""):match("^found %d+ outside (%d+)$")
  if not outside then raise("no_output") end
  local items, offered = {}, {}
  for place, said in rows:gmatch("([^\t\n]+)\t([^\n]*)\n") do
    items[#items + 1] = { id = place, title = place, subtitle = said ~= "" and said or nil, icon = "document" }
    offered[place] = true
  end
  if #items == 0 then raise(tonumber(outside) > 0 and "outside_only" or "no_files") end
  local place = args.choice
  if place then
    if not offered[place] then raise("not_offered") end
  elseif #items == 1 then
    place = items[1].id
  else
    local why
    place, why = rex.ui.pick{ title = "Open file from terminal output", placeholder = "Filter files and messages",
      items = items }
    if not place then
      if why == "cancelled" then return end
      raise("no_picker")
    end
  end
  show_place(ctx, source, place)
end
rex.action{ name = "milo_open_output", title = "Milo: Open File from Terminal Output…", args = { choice = "string?" },
  description = "Open a project file, at the line printed with it, from this pane's recent output.",
  keywords = "error diagnostic compiler path line reader",
  run = function(ctx, args) return open_output(ctx, args or {}) end }
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
