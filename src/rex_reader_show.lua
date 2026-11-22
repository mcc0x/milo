-- Bring a floating reader forward for the file it was just asked to show, and name it after that file.
local a = rex.args
local shown, problem = rex.call("session.focus_block", {session_id = rex.session_id, block_id = a.block})
if not shown then error(problem or "not shown") end
if a.label then
  rex.call("session.set_block_label", {session_id = rex.session_id, block_id = a.block, label = a.label})
end
