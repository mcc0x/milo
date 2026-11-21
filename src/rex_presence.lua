-- This script runs inside the local Rex CLI for Milo's Discord presence
-- helper. With no arguments it reports one fact: whether any Rex app has a
-- session open. It then asks Rex for the connected apps only, and never for a
-- session's label, its tabs, its terminals or anything they hold.
--
-- The person can choose to show the name of the session in front, the Git
-- project in its focused pane, or both. Only then does the script look at
-- sessions, and only at these: which session is in front, its label if the
-- session name was chosen, and where the program in its focused pane is
-- working if the project was chosen. It never reads a tab's or a terminal's
-- title, a screen, a command line or a program's status text.
local want_session, want_directory = rex.args.session == true, rex.args.directory == true
local detailed = want_session or want_directory
local terminal = "com.superlogical.terminal"
local last

local function hex(text)
  return (text:gsub(".", function(byte) return string.format("%02x", byte:byte()) end))
end

-- Whether any Rex app has a session open, and which of those apps run on
-- this computer: the ones that reached the server over its local socket.
local function apps()
  local listed = rex.call("client.list", { kinds = { "app" } })
  if not listed then return end
  local active, here = false, {}
  for _, client in ipairs(listed.clients or {}) do
    -- An app whose connection dropped may come back, but nobody is using it now.
    if client.connection_state ~= "lost" and type(client.session_ids) == "table" and #client.session_ids > 0 then
      active = true
      local principal = client.principal
      if type(principal) == "table" and principal.transport == "unix" then here[client.client_id] = true end
    end
  end
  return active, here
end

local function focused_block(view)
  for _, window in ipairs(view.windows or {}) do
    if window.window_id == view.active_window_id then
      for _, layer in ipairs(window.layers or {}) do
        for _, block in ipairs(layer.blocks or {}) do
          if block.block_id == window.focused_block_id then return block end
        end
      end
    end
  end
end

-- The session in front: the only one whose active tab has its focused pane
-- held by a Rex app on this computer. Rex has no call that names the session
-- an app is showing, so when two sessions fit, or none, there is no answer,
-- and no name is reported rather than the name of the wrong session.
local function front(here)
  local listed = rex.call("session.list")
  local found
  for _, session in ipairs(listed and listed.sessions or {}) do
    local view = rex.session.view{ session_id = session.session_id }
    local block = view and focused_block(view)
    if block and block.owner and here[block.owner] then
      if found then return end
      found = { session = session, block = block }
    end
  end
  return found
end

-- Where the program in the pane is working, as a folder Milo can look for a Git project in.
local function directory(found)
  if found.block.creator_name ~= terminal then return end
  local process = rex.call(terminal .. ".process", { session_id = found.session.session_id, block_id = found.block.block_id })
  local running = process and (process.foreground or process.child)
  local cwd = running and running.cwd
  if type(cwd) == "string" and cwd:sub(1, 1) == "/" and #cwd <= 4096 and not cwd:find("%z") then return cwd end
end

local function report()
  local active, here = apps()
  if active == nil then return end
  local line = "milo-rex idle"
  if active then
    line = "milo-rex active"
    local found = detailed and front(here)
    if found then
      local label = want_session and found.session.label
      if type(label) ~= "string" or label == "" or #label > 256 or label:find("%z") then label = nil end
      local cwd = want_directory and directory(found)
      line = line .. " " .. (label and hex(label) or "-") .. " " .. (cwd and hex(cwd) or "-")
    end
  end
  if line == last then return end
  last = line
  io.stdout:write(line, "\n")
  io.stdout:flush()
end

for _, event in ipairs{
  "client_joined", "client_left", "client_changed",
  "client_connected", "client_disconnected", "client_connection_changed",
  "session_created", "session_destroyed",
} do
  rex.on(event, report)
end

if detailed then
  -- Many of these arrive together, as when a tab is switched or a command
  -- starts. Look once they have settled, and say something only if it changed.
  local pending
  local function settle()
    if pending or not rex.after then return end
    pending = rex.after(1, function()
      pending = nil
      report()
    end)
  end
  local events = { "session_view_changed", "active_window_changed", "block_owner_changed" }
  if want_session then events[#events + 1] = "session_label_changed" end
  if want_directory then
    events[#events + 1] = "terminal.process_changed"
    events[#events + 1] = "terminal.pwd_changed"
  end
  for _, event in ipairs(events) do rex.on(event, settle) end
end

-- Sessions cannot be read until the script has attached, which is after its
-- last line. Wait for that when names are wanted, so the first report has them.
if detailed and rex.after then rex.after(0, report) else report() end
