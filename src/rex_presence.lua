-- This script runs inside the local Rex CLI for Milo's Discord presence
-- helper. It reports one fact: whether any Rex app has a session open. It
-- asks Rex for the connected apps only, and never for a session's label, its
-- tabs, its terminals or anything they hold.
local last

local function report()
  local listed = rex.call("client.list", { kinds = { "app" } })
  if not listed then return end
  local active = false
  for _, client in ipairs(listed.clients or {}) do
    -- An app whose connection dropped may come back, but nobody is using it now.
    if client.connection_state ~= "lost" and type(client.session_ids) == "table" and #client.session_ids > 0 then
      active = true
      break
    end
  end
  if active == last then return end
  last = active
  io.stdout:write(active and "milo-rex active\n" or "milo-rex idle\n")
  io.stdout:flush()
end

for _, event in ipairs{
  "client_joined", "client_left", "client_changed",
  "client_connected", "client_disconnected", "client_connection_changed",
  "session_created", "session_destroyed",
} do
  rex.on(event, report)
end

report()
