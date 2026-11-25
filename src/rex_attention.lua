-- This script runs inside the local Rex CLI for the Milo menu's "Needs you"
-- section, and only while that section is turned on. It reports which
-- terminals hold a program status that asks for a person: blocked, error or
-- done. Of a status record it passes on the record's ID, state, kind, program
-- name and time. It never reads a record's title or message, a terminal's
-- title, screen or command line, and it sends nothing to the terminal.
--
-- It answers only for the Rex server on this computer, reached over its local
-- socket, and only for terminals that server runs itself. A block another
-- host runs is left out rather than guessed at.
local terminal = "com.superlogical.terminal"
local actionable = { blocked = true, error = true, done = true }
local host
-- The session of each block last reported with records, by block ID.
local shown = {}

local function emit(report)
  io.stdout:write("milo-attention ", rex.json.encode(report), "\n")
  io.stdout:flush()
end

local function clear(block_id)
  if not shown[block_id] then return end
  shown[block_id] = nil
  emit{ block = block_id, clear = true }
end

-- The tab that holds the block, if the block is a terminal this server runs.
local function tab_of(view, block_id)
  for _, window in ipairs(view.windows or {}) do
    for _, layer in ipairs(window.layers or {}) do
      for _, block in ipairs(layer.blocks or {}) do
        if block.block_id == block_id then
          if block.creator_name == terminal and block.host_id == host then return window end
          return nil
        end
      end
    end
  end
end

-- Says what the block's records ask for now. `view` is its session's, when the caller has it.
local function report(session_id, block_id, view)
  if not host then return end
  local status = rex.call(terminal .. ".program_status", { session_id = session_id, block_id = block_id })
  local records = {}
  for _, record in ipairs(status and status.records or {}) do
    if actionable[record.state] then
      records[#records + 1] = {
        id = record.id or "",
        state = record.state,
        kind = record.kind,
        app = record.effective_app or record.app,
        at = record.updated_at,
      }
    end
  end
  if #records == 0 then return clear(block_id) end
  view = view or rex.session.view{ session_id = session_id }
  local tab = view and tab_of(view, block_id)
  if not tab then return clear(block_id) end
  shown[block_id] = session_id
  emit{
    block = block_id, session = session_id, window = tab.window_id,
    name = view.label, tab = tab.label, records = records,
  }
end

local function relabel(ctx)
  for block_id, session_id in pairs(shown) do
    if session_id == ctx.session_id then report(session_id, block_id) end
  end
end

rex.on("terminal.program_status_changed", function(ctx, event)
  local record = event and event.record
  if shown[ctx.block_id] or (record and actionable[record.state]) then report(ctx.session_id, ctx.block_id) end
end)
rex.on("terminal.program_status_removed", function(ctx)
  if shown[ctx.block_id] then report(ctx.session_id, ctx.block_id) end
end)
rex.on("block_closed", function(_, event) clear(event.block_id) end)
rex.on("session_destroyed", function(_, event)
  for block_id, session_id in pairs(shown) do
    if session_id == event.session_id then clear(block_id) end
  end
end)
rex.on("session_label_changed", relabel)
rex.on("window_label_changed", relabel)

-- Sessions cannot be read until the script has attached, which is after its last line.
rex.after(0, function()
  local who, status = rex.call("server.whoami"), rex.call("server.status")
  local principal = who and who.principal
  local local_socket = type(principal) == "table" and principal.kind == "peer" and principal.transport == "unix"
  if not local_socket or type(status) ~= "table" or type(status.host_id) ~= "string" then
    emit{ remote = true }
    return rex.stop(0)
  end
  host = status.host_id
  emit{ server = host }
  local listed = rex.call("session.list")
  for _, session in ipairs(listed and listed.sessions or {}) do
    local view = rex.session.view{ session_id = session.session_id }
    for _, window in ipairs(view and view.windows or {}) do
      for _, layer in ipairs(window.layers or {}) do
        for _, block in ipairs(layer.blocks or {}) do
          if block.creator_name == terminal and block.host_id == host then
            report(session.session_id, block.block_id, view)
          end
        end
      end
    end
  end
  emit{ ready = true }
end)
