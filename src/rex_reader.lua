-- Replace a floating reader only after Rex has created its successor.
local a = rex.args
local info = rex.session.describe_block{block_id = rex.block_id}
if not (info and info.block and info.block.window_id) then return end
local opened = rex.session.new_layer{
  window_id = info.block.window_id, bounds = {0.12, 0.08, 0.76, 0.84}, focus = true,
  layout = {block = {flavor = "com.superlogical.terminal.shell", label = a.label,
    options = {command = a.command, shell = "none", cwd = a.cwd}}},
}
if not (opened and opened.layer_id) then return end
if a.previous and a.previous ~= opened.layer_id then
  -- Closing the old reader by hand is normal; its absence isn't an open failure.
  rex.session.close_layer{layer_id = a.previous}
end
print(opened.layer_id)
