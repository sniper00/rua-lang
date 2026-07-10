-- Harness for the `.ruai` interop demo. Run from the workspace root:
--   lua tests/fixtures/examples/rua_moon/run.lua
--
-- It stands in for the moon_rs host `moon` global so main.lua (generated from
-- main.rua, which imports moon.ruai) runs under a plain Lua 5.5 interpreter.
-- Under moon_rs these functions are provided natively.

moon = {
    log = function(msg) print("[moon.log] " .. msg) end,
    error = function(msg) error(msg) end,
    time = function() return 1720000000 end,
    clock = function() return os.clock() end,
    sleep = function(_) end,
}

package.path = package.path .. ";lualib/?.lua"
dofile("tests/fixtures/examples/rua_moon/main.lua")
