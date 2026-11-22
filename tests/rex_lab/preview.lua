-- Run only in the disposable session created by the Rex lab.
local version = assert(rex.call("server.version"))
print(version.rex_version, version.commit, version.platform, "API " .. version.api_revision)
local missing = "org.example.spike.webkit" -- deliberately unregistered, not a proposed Rex API name
local creators = assert(rex.call("block_creator.list"))
for _, creator in ipairs(creators.creators) do
  for _, flavor in ipairs(creator.flavors) do
    print(flavor.flavor, flavor.description)
    assert(flavor.flavor ~= missing, "the negative control must not be registered")
  end
end

local function window()
  local view = assert(rex.session.view{})
  assert(#view.windows == 1)
  return view.windows[1]
end
local anchor = window().focused_block_id
local function split(flavor)
  return rex.session.new_split{
    anchor_id = anchor, direction = "horizontal", side = "after", focus = false,
    layout = { block = { flavor = flavor } },
  }
end

-- A successful split only proves placement, not that the client can render HTML.
local onboarding = "com.superlogical.onboarding.onboarding"
local made = assert(split(onboarding))
local function check_control()
  local w = window()
  assert(w.focused_block_id == anchor)
  local blocks = w.layers[1].blocks
  assert(#blocks == 2 and blocks[2].block_id == made.block_ids[1] and blocks[2].flavor == onboarding)
end
check_control()
local result, err = split(missing)
assert(not result and err, "Rex accepted an unregistered flavor")
print(err)
check_control()
