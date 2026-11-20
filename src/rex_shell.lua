-- This script runs inside the local Rex CLI. Only emit framed updates for
-- the selected shell; unrelated terminals never reach the Milo pane.
local anchor, pane = rex.args.anchor, rex.args.pane
local target = { session_id = rex.session_id, block_id = anchor }
local terminal = "com.superlogical.terminal."
local shell_names = {
  sh = true, bash = true, zsh = true, fish = true, dash = true, ksh = true,
  mksh = true, csh = true, tcsh = true, nu = true,
}

local function login_shell(process)
  return process and type(process.argv0) == "string" and process.argv0:sub(1, 1) == "-"
end

local function shell_of(process)
  local child = process.child
  if child and (login_shell(child) or shell_names[child.name]) then return child end
  -- On macOS the direct child may be `login`; an editor in the foreground
  -- does not control which directory this tool shows.
  local ancestors = process.ancestors or {}
  for i = #ancestors, 1, -1 do
    if login_shell(ancestors[i]) then return ancestors[i] end
  end
  if login_shell(process.foreground) then return process.foreground end
end

local last, reporter
local function same_process(a, b)
  return a and b and a.pid and a.name and a.pid == b.pid and a.name == b.name
end

local function emit(cwd)
  if type(cwd) ~= "string" or cwd:sub(1, 1) ~= "/" or #cwd > 16384 then cwd = false end
  if cwd == last then return end
  last = cwd
  local encoded = cwd and cwd:gsub(".", function(byte) return string.format("%02x", byte:byte()) end) or "-"
  io.stdout:write("milo-cwd ", encoded, "\n")
  io.stdout:flush()
end

local function sample(process, report)
  local shell = process and shell_of(process)
  if not same_process(reporter, shell) then reporter = nil end
  if report and type(report.pwd) == "string" and report.pwd ~= "" and same_process(report.owner, shell) then
    reporter = report.owner
  end
  -- A known local shell in the terminal's process tree has its own cwd
  -- even when the prompt never emits a directory report. Never substitute
  -- the foreground app's cwd or a path from an OSC 7 URI.
  emit(shell and (reporter or shell_names[shell.name]) and shell.cwd)
end

rex.on("terminal.pwd_changed", function(ctx, event)
  if ctx.block_id == anchor then sample(rex.call(terminal .. "process", target), event) end
end)
rex.on("terminal.process_changed", function(ctx)
  if ctx.block_id == anchor then
    local current = rex.call(terminal .. "process", target)
    sample(current, rex.call(terminal .. "pwd", target))
  end
end)
rex.on("terminal.child_exited", function(ctx)
  if ctx.block_id == anchor or ctx.block_id == pane then rex.stop() end
end)
rex.on("block_closed", function(_, event)
  if event.block_id == anchor or event.block_id == pane then rex.stop() end
end)

local initial = rex.call(terminal .. "process", target)
if not initial then rex.stop() end
sample(initial, rex.call(terminal .. "pwd", target))
