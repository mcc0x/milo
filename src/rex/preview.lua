-- Runs in Rex's local CLI. Do not read terminal content, invoke client
-- actions, or ask another server. Native code independently proves geometry.
local a = rex.args
local terminal = "com.superlogical.terminal"
local preview_label, staging_label = "milo:preview", "milo:preview:starting"
local session = { session_id = a.session }
local function exact(id, kind)
  if type(id) ~= "string" or id:sub(1, #kind) ~= kind then return false end
  local value = id:sub(#kind + 1)
  return #value == 36 and value:match("^%x+%-%x+%-%x+%-%x+%-%x+$")
    and value:sub(9, 9) == "-" and value:sub(14, 14) == "-"
    and value:sub(19, 19) == "-" and value:sub(24, 24) == "-"
end

local function valid_pid(pid)
  return type(pid) == "number" and pid >= 2 and pid <= 2147483647 and pid % 1 == 0
end

local function peer(principal)
  return principal and principal.kind == "peer" and principal.transport == "unix"
    and principal.uid == a.uid and valid_pid(principal.pid)
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
      return status.host_id, principal.pid
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

local remote_names = { ssh = true, sshd = true, ["sshd-session"] = true, mosh = true, ["mosh-client"] = true }

local function process_parent(pid)
  if not valid_pid(pid) then return end
  local reply = rex.exec({ "/bin/ps", "-o", "ppid=,uid=,ucomm=", "-p", tostring(pid) },
    { clear_env = true, timeout = 1 })
  if not (reply and reply.ok) then return end
  local ppid, uid, name = reply.stdout:match("^%s*(%d+)%s+(%d+)%s+(.-)%s*$")
  return tonumber(ppid), tonumber(uid), name
end

local function request_from_milo(cli_pid)
  local opener, uid = process_parent(cli_pid)
  return valid_pid(a.pid) and opener == a.pid and uid == a.uid
end

local function own_caller(caller, cli_pid)
  -- The Rex CLI was started directly by the Milo process that made the
  -- request. Trace that process to the child Rex launched for this exact PTY,
  -- not to a PID inferred from focus or even another working terminal.
  if not request_from_milo(cli_pid) then return false end
  local info = process(caller)
  local root = info and not info.last_exit and info.child
  if not (root and valid_pid(root.pid)) then return false end
  if remote_names[root.name] then return false, true end
  local login = root.path == "/usr/bin/login" and root.user_id == 0
  if not login and root.user_id ~= a.uid then return false end
  local pid = a.pid
  for _ = 1, 24 do
    if pid == root.pid then return not login or pid ~= a.pid end
    local parent, owner, name = process_parent(pid)
    if not parent or owner ~= a.uid or not name then return false end
    if remote_names[name] then return false, true end
    pid = parent
  end
  return false
end

local function own_anchor(block)
  local info = process(block)
  local root = info and not info.last_exit and info.child
  if not root then return false end
  if root.pid == a.pid and root.user_id == a.uid then return true end
  -- A macOS login terminal can report only its root-owned login wrapper.
  -- Accept the anchor if it is that wrapper's actual direct child.
  if root.path == "/usr/bin/login" and root.user_id == 0 then
    local parent, owner = process_parent(a.pid)
    return parent == root.pid and owner == a.uid
  end
  return false
end

local shell_names = { sh = true, bash = true, zsh = true, fish = true, dash = true,
  ksh = true, mksh = true, csh = true, tcsh = true, nu = true }
local function command_line(child)
  local pid = child.pid
  if not valid_pid(pid) then return end
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
  local chain = processes(info)
  for _, child in ipairs(chain) do
    if remote_names[child.name] then return nil, true end
  end
  -- Rex's ancestors are nearest-first; processes() has already put them
  -- in root-to-foreground order. Pick the deepest interactive shell only.
  for i = #chain, 1, -1 do
    local child = chain[i]
    if shell_names[child.name] and child.user_id == a.uid and type(child.cwd) == "string"
      and child.cwd:sub(1, 1) == "/" and interactive(child) then return child.cwd end
  end
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

local function working_pair(host, main, window, layer)
  return window and window.window_id == a.window and layer.kind == "tiled"
    and local_terminal(main, host) and main.owner == a.owner
end

local function cancel_staged(host, id)
  local described = rex.session.describe_block{ session_id = a.session, block_id = id }
  local block = described and described.block
  if local_terminal(block, host) and block.block_id == id and block.label == staging_label then
    rex.call("block.close", { session_id = a.session, block_id = id, keep_last = true })
  end
end

local function publish(host, id)
  local view = current_view()
  if view and view.active_window_id == a.window then
    local main, window, tiled = locate(view, a.main)
    local caller, origin, caller_layer = locate(view, a.block)
    local block, place, layer = locate(view, id)
    if working_pair(host, main, window, tiled) and origin == window and caller_layer.kind == "tiled"
      and local_terminal(caller, host) and caller.owner == a.owner and not window.zoomed_block_id
      and place == window and layer.kind == "tiled" and local_terminal(block, host) and block.label == staging_label
      and local_owner(a.owner) == a.owner_pid
      and rex.session.set_block_label{ session_id = a.session, block_id = id, label = preview_label } then
      return "ok"
    end
  end
  cancel_staged(host, id)
  return "changed"
end

local function open(host, view, caller, window, layer, owner_pid)
  if window.zoomed_block_id then return "zoom" end
  if layer.kind ~= "tiled" then return "floating_caller" end
  local main, cwd = working_terminal(view, caller, window, layer, host)
  if not main then return cwd end
  if main.owner ~= caller.owner then return "client" end
  if not unchanged(view) then return "tab" end
  if a.mode == "identify" then
    return rex.json.encode({ main = main.block_id, window = window.window_id, cwd = cwd,
      owner = caller.owner, owner_pid = owner_pid })
  end
  if window.window_id ~= a.window or main.block_id ~= a.main or cwd ~= a.cwd
    or caller.owner ~= a.owner or owner_pid ~= a.owner_pid then return "changed" end
  local direction = directions[a.placement]
  if not direction then return "placement" end
  local layout = { block = { flavor = terminal .. ".shell", label = staging_label, options = {
    command = { a.exe, "preview-anchor", a.url, a.main, a.window, a.owner, tostring(a.owner_pid) },
    shell = "none", cwd = cwd,
    -- Rex also keeps quick successful exits. Milo displays failures itself
    -- for a bounded time; Rex should close the anchor whenever Milo exits.
    exit = { on_completion = true, quick_exit_threshold_ms = 0 },
  } } }
  -- Rex has no if-active-window or revision precondition for these mutations.
  -- Recheck the created block and original placement before publishing the
  -- label that permits its watcher to show. Never close anything but our
  -- exact unpublished terminal if the layout changed during the request.
  local made = rex.session.new_split{ session_id = a.session, anchor_id = main.block_id,
    direction = direction[1], side = direction[2], focus = false, layout = layout }
  local id = made and made.block_ids and #made.block_ids == 1 and made.block_ids[1]
  return exact(id, "block:") and publish(host, id) or "placement"
end

local function focus(host, view, caller, window, cli_pid)
  local main, place, layer = locate(view, a.main)
  if a.main == a.block or place ~= window or not working_pair(host, main, place, layer)
    or caller.label ~= preview_label or not request_from_milo(cli_pid) or not own_anchor(caller) then return "focus" end
  if window.zoomed_block_id and window.zoomed_block_id ~= a.main then return "focus" end
  if not unchanged(view) then return "tab" end
  -- Rex doesn't offer a conditional focus RPC either; the view check above
  -- cannot be atomic with the action. Never issue a second focus to repair it.
  return rex.session.focus_block{ session_id = a.session, block_id = a.main } and "ok" or "focus"
end

local function one_shot(host, cli_pid)
  if not host then return "transport" end
  local view = current_view()
  if not view then return "tab" end
  local caller, window, layer = locate(view, a.block)
  if not caller or window.window_id ~= view.active_window_id then return "tab" end
  if not local_terminal(caller, host) then return "remote" end
  local owner_pid = local_owner(caller.owner)
  if not owner_pid then return "client" end
  if a.mode == "focus" then
    if layer.kind ~= "tiled" or caller.owner ~= a.owner or owner_pid ~= a.owner_pid then return "focus" end
    return focus(host, view, caller, window, cli_pid)
  end
  local belongs, remote = own_caller(caller, cli_pid)
  if not belongs then return remote and "remote" or "caller" end
  return open(host, view, caller, window, layer, owner_pid)
end

local function visible(view, window, layer, index)
  if window.window_id ~= view.active_window_id or layer.state == "minimized" then return false end
  local zoom = window.zoomed_block_id
  if zoom and zoom ~= a.block then return false end
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

local host, cli_pid = local_host()
local last, verified, published, last_revision
local function emit(pid)
  local state = pid and ("show " .. tostring(pid) .. " tile") or "hide"
  if last ~= state then
    last = state
    io.stdout:write(state, "\n")
    io.stdout:flush()
  end
end

local function stop(code)
  emit()
  rex.stop(code or 0)
end

if not host or not exact(a.owner, "client:") or not valid_pid(a.owner_pid)
  or not exact(a.window, "window:") or not exact(a.main, "block:") or a.main == a.block
  or not request_from_milo(cli_pid) then
  stop(1)
  return
end

local function track_anchor(block)
  if not local_terminal(block, host) or block.block_id ~= a.block
    or (block.label ~= preview_label and block.label ~= staging_label) then return false end
  if not verified then
    if not own_anchor(block) then return false end
    verified = true
  end
  if block.label == preview_label then published = true end
  return true
end

local function refresh()
  local view = current_view()
  if not view then return stop(1) end
  last_revision = view.revision
  local block, window, layer, index = locate(view, a.block)
  local main, place, tiled = locate(view, a.main)
  if block and layer.kind ~= "tiled" then return stop() end
  if not block or not main then
    local listed = rex.session.list_blocks(session)
    if not listed then return stop(1) end
    local function detached(id)
      for _, other in ipairs(listed.blocks or {}) do
        if other.block_id == id then return other.placement == "detached" end
      end
      return false
    end
    if (block or detached(a.block)) and (main or detached(a.main)) then
      -- Publication is independent of placement. A detached anchor has
      -- no view entry, but Rex still describes its exact block and label.
      if not block then
        local described = rex.session.describe_block{ session_id = a.session, block_id = a.block }
        block = described and described.block
      end
      if not track_anchor(block) then return stop(1) end
      return emit()
    end
    return stop()
  end
  if window.window_id ~= a.window or place ~= window then return stop() end
  if not local_terminal(main, host) or tiled.kind ~= "tiled" or not track_anchor(block) then return stop(1) end
  if block.label ~= preview_label then return emit() end
  if not visible(view, window, layer, index) or block.owner ~= a.owner or main.owner ~= a.owner
    or local_owner(a.owner) ~= a.owner_pid then return emit() end
  emit(a.owner_pid)
end

rex.on("session_view_changed", function(ctx, event)
  if ctx.session_id ~= a.session then return end
  if type(event.revision) == "number" and type(last_revision) == "number"
    and event.revision <= last_revision then return end
  refresh()
end)
for _, event in ipairs{ "active_window_changed", "block_owner_changed", "client_connected" } do
  rex.on(event, function(ctx) if ctx.session_id == a.session then refresh() end end)
end
rex.on("client_disconnected", function(ctx, event)
  if ctx.session_id == a.session and event.client_id == a.owner then stop() end
end)
rex.on("client_connection_changed", function(ctx, event)
  if ctx.session_id == a.session then
    if event.client_id == a.owner and event.connection_state ~= "connected" then stop() else refresh() end
  end
end)
rex.on("block_closed", function(ctx, event)
  if ctx.session_id == a.session and (event.block_id == a.block or event.block_id == a.main) then stop() end
end)
rex.on("window_closed", function(ctx, event)
  if ctx.session_id == a.session and event.window_id == a.window then stop() end
end)
rex.on("terminal.child_exited", function(ctx)
  if ctx.session_id == a.session and ctx.block_id == a.block then stop() end
end)

emit()
-- A script attaches to its scoped session after the top-level chunk returns.
rex.after(0, refresh)
-- If the launcher disappears before it can validate and publish our block,
-- close the unused terminal without ever exposing an overlay.
rex.after(8, function()
  if published then return end
  local described = rex.session.describe_block{ session_id = a.session, block_id = a.block }
  if not track_anchor(described and described.block) then return stop(1) end
  if not published then stop() end
end)
