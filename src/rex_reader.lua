-- Replace a floating reader only after Rex has created its successor.
local a = rex.args
local info = rex.session.describe_block{block_id = rex.block_id}
if not (info and info.block and info.block.window_id) then return end
local command = a.command
if a.preview_origin then
  -- Carry the originating tab and pane into a Reader, not the current focus.
  local origin = a.preview_origin == "" and "caller " .. rex.block_id or "shell " .. a.preview_origin
  local source = table.concat({rex.session_id, info.block.window_id, origin}, " ")
  command = {"/usr/bin/env", "MILO_PREVIEW_SOURCE=" .. source}
  for _, word in ipairs(a.command) do command[#command + 1] = word end
end
local opened = rex.session.new_layer{
  window_id = info.block.window_id, bounds = {0.12, 0.08, 0.76, 0.84}, focus = true,
  layout = {block = {flavor = "com.superlogical.terminal.shell", label = a.label,
    options = {command = command, shell = "none", cwd = a.cwd}}},
}
local block = opened and opened.block_ids and opened.block_ids[1]
if not block then return end
if a.previous and a.previous ~= block then
  -- The old block may already be gone, or share its layer with other blocks.
  rex.call("block.close", {session_id = rex.session_id, block_id = a.previous})
end
print(block)
