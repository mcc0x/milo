-- With Lua 5.1: lua src/rex_reader_test.lua src/rex_reader.lua
local reader = assert(loadfile(assert(arg[1])))
local opened = {layer_id = "layer:new", block_ids = {"block:new"}}

local function attempt(result, previous, old_exists)
  local created, printed, closed
  setfenv(reader, {
    print = function(id) printed = id end,
    rex = {
      session_id = "session:test",
      block_id = "block:outline",
      args = {previous = previous},
      session = {
        describe_block = function(args)
          assert(args.block_id == "block:outline")
          return {block = {window_id = "window:test"}}
        end,
        new_layer = function(args)
          assert(args.window_id == "window:test" and args.focus)
          created = result
          return result
        end,
        close_layer = function() error("the old layer contains another pane") end,
      },
      call = function(method, args)
        assert(created == opened and not closed)
        assert(method == "block.close" and args.session_id == "session:test" and args.block_id == "block:old")
        closed = args.block_id
        if old_exists then return {} end
        return nil, "not_found"
      end,
    },
  })
  reader()
  assert(printed == (result == opened and "block:new" or nil))
  assert(closed == (result == opened and previous == "block:old" and "block:old" or nil))
end

attempt(opened, nil, false)
attempt(opened, "block:old", true)
attempt(opened, "block:old", false)
attempt(nil, "block:old", true)
attempt({layer_id = "layer:new", block_ids = {}}, "block:old", true)
