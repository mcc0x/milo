-- Invoked on the local CLI, never as an installed Rex script. Only the
-- block ID returned by an earlier Milo split is eligible for replacement.
local a = rex.args
-- An older Rex names the caller's session and it must still be that one. A
-- newer Rex names only the block, and the CLI finds the session holding it.
local session = a.session or rex.session_id
local flavor = "com.superlogical.terminal.shell"
local function exact(id, kind)
  if type(id) ~= "string" or id:sub(1, #kind) ~= kind then return false end
  local uuid = id:sub(#kind + 1)
  return #uuid == 36 and uuid:sub(9, 9) == "-" and uuid:sub(14, 14) == "-"
    and uuid:sub(19, 19) == "-" and uuid:sub(24, 24) == "-"
    and #uuid:gsub("%x", "") == 4
end

local function local_server()
  local who = rex.call("server.whoami", {})
  local peer = who and who.principal
  if not (peer and peer.kind == "peer" and peer.transport == "unix" and peer.uid == a.uid) then return end
  local status = rex.call("server.status", {})
  if status and exact(status.instance_id, "") and exact(status.host_id, "") then return status end
end

local function tab()
  local view = rex.session.view{session_id = session}
  if not (view and view.session_id == session) then return end
  for _, window in ipairs(view.windows or {}) do
    local tiled, caller_kind
    for _, layer in ipairs(window.layers or {}) do
      if layer.kind == "tiled" then tiled = layer end
      for _, block in ipairs(layer.blocks or {}) do
        if block.block_id == a.caller then caller_kind = layer.kind end
      end
    end
    if caller_kind and tiled and exact(window.window_id, "window:") then return window, tiled, caller_kind end
  end
end

local function owned_or_shell(tiled, id, host)
  if not exact(id, "block:") then return end
  for _, pane in ipairs(tiled.blocks or {}) do
    if pane.block_id == id and pane.flavor == flavor and pane.host_id == host then return id end
  end
end

local function anchor(window, tiled, caller_kind, host, old)
  local id, explicit = a.caller, false
  if a.anchor then id, explicit = a.anchor, true end
  if caller_kind ~= "tiled" and not explicit then
    local source = a.source
    if caller_kind ~= "floating" or not source or source.session ~= session
      or source.window ~= window.window_id then return end
    id, explicit = source.block, source.explicit
  end
  if id == old or not owned_or_shell(tiled, id, host) then return end
  if not explicit then
    -- Rex does not distinguish an interactive shell from a direct command
    -- in its layout. Never guess which of several terminals owns the file.
    for _, pane in ipairs(tiled.blocks or {}) do
      if pane.flavor == flavor and pane.block_id ~= id and pane.block_id ~= old then return end
    end
  end
  return id
end

if not exact(session, "session:") or rex.session_id ~= session or rex.block_id ~= a.caller then
  return print("context")
end
local server = local_server()
if not server then return print("context") end
local window, tiled, caller_kind = tab()
if not window then return print("context") end
if not a.window then return print(server.instance_id, window.window_id, session) end
if a.instance ~= server.instance_id or a.window ~= window.window_id then return print("context") end
if window.zoomed_block_id then return print("zoomed") end
local old = owned_or_shell(tiled, a.old, server.host_id)
local shell = anchor(window, tiled, caller_kind, server.host_id, old)
if not shell then return print("anchor") end

local made = rex.session.new_split{
  session_id = session, anchor_id = shell, direction = "horizontal", side = "after", ratio = 0.6, focus = true,
  layout = {block = {flavor = flavor, label = "milo:markdown-preview",
    options = {shell = "none", cwd = "/", command = a.command}}},
}
local opened = made and made.block_ids and made.block_ids[1]
if not exact(opened, "block:") or opened == old or made.anchor_id ~= shell then return print("failed") end
if old then
  -- A pane can be moved or closed while the split starts. Never use a
  -- detached ID, another tab, a floating layer, or a label as ownership.
  local current, current_tiled = tab()
  if current and current.window_id == a.window and owned_or_shell(current_tiled, opened, server.host_id)
    and owned_or_shell(current_tiled, old, server.host_id) then
    local closed = rex.call("block.close", {session_id = session, block_id = old, keep_last = true})
    if not closed then
      current, current_tiled = tab()
      if current and current.window_id == a.window and owned_or_shell(current_tiled, old, server.host_id) then
        if owned_or_shell(current_tiled, opened, server.host_id) then
          rex.call("block.close", {session_id = session, block_id = opened, keep_last = true})
        end
        return print("failed")
      end
    end
  end
end
print(opened)
