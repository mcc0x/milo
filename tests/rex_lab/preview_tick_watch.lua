-- Prepended only for the benchmark's counted run. The production script
-- keeps its real Rex functions/results and its own stdout lease protocol.
local rex = (function(api)
  local counters = assert(io.open(@@WATCH_LOG@@, "a"))
  local function count(kind, name)
    assert(counters:write(kind, " ", name, "\n"))
    assert(counters:flush())
  end
  local function request(name, run)
    return function(...)
      count("request", name)
      return run(...)
    end
  end
  local function exec(run, argv, ...)
    local name = argv[1]
    count("exec_attempt", name)
    local result, err = run(argv, ...)
    if result and result.ok then count("exec_ok", name) end
    return result, err
  end
  return setmetatable({
    session = setmetatable({}, {__index = function(self, name)
      local method = request("session." .. name, api.session[name])
      self[name] = method
      return method
    end}),
    call = function(name, ...)
      count("request", name)
      return api.call(name, ...)
    end,
    exec = setmetatable({start = function(argv, ...)
      count("exec_start_attempt", argv[1])
      local result, err = api.exec.start(argv, ...)
      if result then count("exec_started", argv[1]) end
      return result, err
    end}, {__call = function(_, argv, ...) return exec(api.exec, argv, ...) end}),
    on = function(name, callback, ...)
      return api.on(name, function(...)
        count("event", name)
        return callback(...)
      end, ...)
    end,
    after = function(delay, callback, ...)
      return api.after(delay, function(...)
        count("timer", tostring(delay))
        return callback(...)
      end, ...)
    end,
  }, {__index = api})
end)(rex)
