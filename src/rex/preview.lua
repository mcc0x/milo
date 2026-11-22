-- Runs in Rex's local CLI. Do not read terminal content, invoke client
-- actions, or ask another server. Native code independently proves geometry.
local a = rex.args
local terminal = "com.superlogical.terminal"
local session = { session_id = a.session }
local function exact(id, kind)
  if type(id) ~= "string" or id:sub(1, #kind) ~= kind then return false end
  local value = id:sub(#kind + 1)
  return #value == 36 and value:match("^%x+%-%x+%-%x+%-%x+%-%x+$")
    and value:sub(9, 9) == "-" and value:sub(14, 14) == "-"
    and value:sub(19, 19) == "-" and value:sub(24, 24) == "-"
end

local function peer(principal)
  return principal and principal.kind == "peer" and principal.transport == "unix"
    and principal.uid == a.uid and type(principal.pid) == "number"
    and principal.pid >= 2 and principal.pid <= 2147483647 and principal.pid % 1 == 0
end

local function local_host()
  if rex.session_id ~= a.session or rex.block_id ~= a.block then return end
  local identity = rex.call("server.whoami")
  local principal = identity and identity.principal
  if not peer(principal) then return end
  -- The CLI itself must be the peer, not the process at the far end of an
  -- SSH StreamLocalForward or another Unix socket relay. This fixed command
  -- runs locally in rex do; it never includes any terminal data or user input.
  local check = rex.exec({ "/bin/sh", "-c", [[printf '%s' "$PPID"]] }, { clear_env = true, timeout = 1 })
  if not (check and check.ok and tonumber(check.stdout) == principal.pid) then return end
  local status = rex.call("server.status")
  if not (status and exact("host:" .. (status.host_id or ""), "host:")) then return end
  for _, listener in ipairs(status.listeners or {}) do
    if listener.endpoint == a.server and listener.transport == "unix" and listener.admission == "peer_credentials" then
      return status.host_id
    end
  end
end

local function local_terminal(block, host)
  return block and block.creator_name == terminal and block.host_id == host
end

local function locate(view, id)
  for _, window in ipairs(view.windows or {}) do
    for i, layer in ipairs(window.layers or {}) do
      for _, block in ipairs(layer.blocks or {}) do
        if block.block_id == id then return block, window, layer, i end
      end
    end
  end
end

local function local_owner(owner)
  if not exact(owner, "client:") then return end
  local listed = rex.session.list_clients(session)
  local pid
  for _, client in ipairs(listed and listed.clients or {}) do
    if client.client_id == owner then
      local info = client.info
      if pid or not (info and info.kind == "app" and info.platform == "macos"
        and client.connection_state == "connected" and peer(client.principal)) then return end
      pid = client.principal.pid
    end
  end
  return pid
end

local function process(block)
  return rex.call(terminal .. ".process", { session_id = a.session, block_id = block.block_id })
end

local function processes(info)
  if not info or info.last_exit then return {} end
  local all = {}
  if info.child then all[#all + 1] = info.child end
  for i = #(info.ancestors or {}), 1, -1 do all[#all + 1] = info.ancestors[i] end
  if info.foreground then all[#all + 1] = info.foreground end
  return all
end

local function own_anchor(block)
  local info = process(block)
  for _, child in ipairs(processes(info)) do
    if child.pid == a.pid and child.user_id == a.uid then return true end
  end
  -- Rex's macOS launch can report only its root-owned login process even
  -- after exec has entered Milo. Verify the known anchor PID underneath it.
  local login = info and info.child
  if login and login.path == "/usr/bin/login" and login.user_id == 0 and type(a.pid) == "number"
    and a.pid >= 2 and a.pid <= 2147483647 and a.pid % 1 == 0 then
    local got = rex.exec({ "/bin/ps", "-o", "ppid=,uid=", "-p", tostring(a.pid) }, { clear_env = true, timeout = 1 })
    if got and got.ok then
      local ppid, uid = got.stdout:match("^%s*(%d+)%s+(%d+)%s*$")
      return tonumber(ppid) == login.pid and tonumber(uid) == a.uid
    end
  end
  return false
end

local shell_names = { sh = true, bash = true, zsh = true, fish = true, dash = true,
  ksh = true, mksh = true, csh = true, tcsh = true, nu = true }
local remote_names = { ssh = true, sshd = true, ["sshd-session"] = true, mosh = true, ["mosh-client"] = true }
local function command_line(child)
  local pid = child.pid
  if type(pid) ~= "number" or pid < 2 or pid > 2147483647 or pid % 1 ~= 0 then return end
  local args = rex.exec({ "/bin/ps", "-ww", "-o", "command=", "-p", tostring(pid) },
    { clear_env = true, timeout = 1 })
  if args and args.ok then return args.stdout:match("^%s*(.-)%s*$") end
end

local interactive_options = { ["--interactive"] = true, ["--login"] = true,
  ["--noprofile"] = true, ["--norc"] = true, ["--no-rcs"] = true, ["--no-config"] = true }
local function interactive(child)
  local argv0 = child.argv0
  if type(argv0) ~= "string" or type(child.path) ~= "string" or child.script
    or not (child.path:sub(1, 1) == "/" and shell_names[child.path:match("([^/]+)$")]) then return false end
  -- A Rex PTY makes a shell with no arguments interactive. For a shell with
  -- arguments, accept only known startup flags, never -c or a script path.
  local command = command_line(child)
  if not command or command:sub(1, #argv0) ~= argv0 then return false end
  local rest = command:sub(#argv0 + 1)
  if rest == "" then return true end
  if not rest:match("^%s") then return false end
  for flag in rest:gmatch("%S+") do
    if not interactive_options[flag] and not flag:match("^%-[il]+$") then return false end
  end
  return true
end

local function shell_cwd(info)
  local cwd
  for _, child in ipairs(processes(info)) do
    if remote_names[child.name] then return nil, true end
    if not cwd and shell_names[child.name] and child.user_id == a.uid and type(child.cwd) == "string"
      and child.cwd:sub(1, 1) == "/" and interactive(child) then
      -- Prefer the first shell starting from the terminal's own child. A
      -- nested foreground shell is not a reason to select another pane.
      cwd = child.cwd
    end
  end
  return cwd
end

local function sidebar_role(block)
  local label = block.label or ""
  local role = label:match("^milo:trial:(%a+)$") or label:match("^milo:(%a+)$")
  if role == "outline" or role == "history" or role == "usage" then return role end
end

local function sidebar_reference(caller, role)
  -- The managed outline/history commands carry --shell as their final pair.
  -- Only inspect the argv of this installation's own Milo child, never argv
  -- of unrelated programs or terminal output; never log the returned text.
  for _, child in ipairs(processes(process(caller))) do
    if (child.path == a.exe or child.invoked_path == a.exe) and child.user_id == a.uid then
      if role == "usage" then return true end -- Usage keeps no --shell reference.
      local command = command_line(child)
      local id = command and command:match(" " .. role .. " %-%-shell (block:[%x%-]+)$")
      if exact(id, "block:") then return id end
      return false
    end
  end
end

local function working_terminal(view, caller, window, tiled, host)
  local role = sidebar_role(caller)
  if not role then
    if (caller.label or ""):sub(1, 5) == "milo:" then return nil, "shell" end
    local cwd, remote = shell_cwd(process(caller))
    if cwd then return caller, cwd end
    return nil, remote and "remote" or "shell"
  end

  local reference = sidebar_reference(caller, role)
  if not reference then return nil, "stale" end
  if type(reference) == "string" then
    local block, place, layer = locate(view, reference)
    if not block or block == caller or place ~= window or layer ~= tiled or sidebar_role(block)
      or (block.label or ""):sub(1, 5) == "milo:" then return nil, "stale" end
    if not local_terminal(block, host) then return nil, "remote" end
    local cwd, remote = shell_cwd(process(block))
    if cwd then return block, cwd end
    return nil, remote and "remote" or "shell"
  end

  local main, cwd
  for _, block in ipairs(tiled.blocks or {}) do
    if block.creator_name == terminal and (block.label or ""):sub(1, 5) ~= "milo:" then
      if not local_terminal(block, host) then return nil, "remote" end
      local path, remote = shell_cwd(process(block))
      if remote then return nil, "remote" end
      if path then
        if main then return nil, "shell" end
        main, cwd = block, path
      end
    end
  end
  return main, cwd or "shell"
end

local function current_view()
  local view = rex.session.view(session)
  if view and view.session_id == a.session then return view end
end

local function unchanged(view)
  local latest = current_view()
  return latest and latest.active_window_id == view.active_window_id and latest.revision == view.revision
end

local directions = {
  right = { "horizontal", "after" }, left = { "horizontal", "before" },
  above = { "vertical", "before" }, below = { "vertical", "after" },
}

local function open(host, view, caller, window, layer)
  if layer.kind ~= "tiled" or window.zoomed_block_id then return "tab" end
  local main, cwd = working_terminal(view, caller, window, layer, host)
  if not main then return cwd end
  if main.owner ~= caller.owner then return "client" end
  if not unchanged(view) then return "tab" end
  if a.mode == "identify" then return rex.json.encode({ main = main.block_id, cwd = cwd }) end
  if main.block_id ~= a.main or cwd ~= a.cwd then return "changed" end
  local layout = { block = { flavor = terminal .. ".shell", label = "milo:preview",
    options = { command = { a.exe, "preview-anchor", a.url, main.block_id }, shell = "none", cwd = cwd } } }
  local made, direction
  if a.placement == "floating" then
    made = rex.session.new_layer{ session_id = a.session, window_id = window.window_id,
      bounds = { 0.15, 0.11, 0.70, 0.78 }, focus = false, layout = layout }
  else
    direction = directions[a.placement]
    if not direction then return "placement" end
    made = rex.session.new_split{ session_id = a.session, anchor_id = main.block_id,
      direction = direction[1], side = direction[2], focus = false, layout = layout }
  end
  return made and made.block_ids and exact(made.block_ids[1], "block:") and "ok" or "placement"
end

local function focus(host, view, caller, window)
  local main, place, layer = locate(view, a.main)
  if a.main == a.block or place ~= window or layer.kind ~= "tiled" or not own_anchor(caller)
    or not local_terminal(main, host) or main.owner ~= caller.owner then return "focus" end
  if window.zoomed_block_id and window.zoomed_block_id ~= a.main then return "focus" end
  if not unchanged(view) then return "tab" end
  return rex.session.focus_block{ session_id = a.session, block_id = a.main } and "ok" or "focus"
end

local function one_shot(host)
  if not host then return "transport" end
  local view = current_view()
  if not view then return "tab" end
  local caller, window, layer = locate(view, a.block)
  if not caller or window.window_id ~= view.active_window_id then return "tab" end
  if not local_terminal(caller, host) then return "remote" end
  if not local_owner(caller.owner) then return "client" end
  if a.mode == "focus" then return focus(host, view, caller, window) end
  return open(host, view, caller, window, layer)
end

local function visible(view, window, layer, index)
  if window.window_id ~= view.active_window_id or layer.state == "minimized" then return false end
  if layer.kind ~= "tiled" and layer.kind ~= "floating" then return false end
  local zoom = window.zoomed_block_id
  if zoom then return layer.kind == "tiled" and zoom == a.block end
  -- Layer order is back to front. Keep the overlay out of any higher visible
  -- layer, even when its individual blocks happen not to overlap right now.
  for i = index + 1, #window.layers do
    if window.layers[i].state ~= "minimized" then return false end
  end
  return true
end

if a.mode ~= "watch" then
  print(one_shot(local_host()))
  return
end

local host = local_host()
local last, anchor_verified, owner, owner_pid
local function emit(pid, layer)
  local state = pid and ("show " .. tostring(pid) .. (layer.kind == "floating" and " float" or " tile")) or "hide"
  if last ~= state then
    last = state
    io.stdout:write(state, "\n")
    io.stdout:flush()
  end
end

local function stop()
  emit()
  rex.stop()
end

if not host then
  emit()
  return
end

local function refresh()
  local view = current_view()
  if not view then return stop() end
  local block, window, layer, index = locate(view, a.block)
  if not block then
    local listed = rex.session.list_blocks(session)
    -- Dragging or detaching can temporarily unplace a living terminal.
    for _, other in ipairs(listed and listed.blocks or {}) do
      if other.block_id == a.block then return emit() end
    end
    return stop()
  end
  if not local_terminal(block, host) then return stop() end
  if not anchor_verified then
    if block.label ~= "milo:preview" or not own_anchor(block) then return stop() end
    anchor_verified = true
  end
  if not visible(view, window, layer, index) then return emit() end
  local pid = local_owner(block.owner)
  if not pid or (owner and (owner ~= block.owner or owner_pid ~= pid)) then return emit() end
  owner, owner_pid = block.owner, pid
  emit(pid, layer)
end

for _, event in ipairs{ "session_view_changed", "active_window_changed", "block_owner_changed", "client_connected" } do
  rex.on(event, function(ctx) if ctx.session_id == a.session then refresh() end end)
end
rex.on("client_disconnected", function(ctx, event)
  if owner and ctx.session_id == a.session and event.client_id == owner then stop() end
end)
rex.on("client_connection_changed", function(ctx, event)
  if ctx.session_id == a.session then
    if owner and event.client_id == owner and event.connection_state ~= "connected" then stop() else refresh() end
  end
end)
rex.on("block_closed", function(ctx, event)
  if ctx.session_id == a.session and event.block_id == a.block then stop() end
end)
rex.on("terminal.child_exited", function(ctx)
  if ctx.session_id == a.session and ctx.block_id == a.block then stop() end
end)

emit()
-- A script attaches to its scoped session after the top-level chunk returns.
rex.after(0, refresh)
