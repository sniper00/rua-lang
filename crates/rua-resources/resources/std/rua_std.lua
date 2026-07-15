local std = { ABI_VERSION = 1 }

local fmt = {}
local number = {}
local result = {}
local iter = {}
local vec = {}
local map = {}
local string_api = {}

std.fmt = fmt
std.number = number
std.result = result
std.iter = iter
std.vec = vec
std.map = map
std.string = string_api

function fmt.format(template, ...)
    local arguments = { ... }
    local index = 0
    local output = template:gsub("{{", "\0LB\0"):gsub("}}", "\0RB\0")
    output = output:gsub("{[:%w%.%?]*}", function()
        index = index + 1
        return tostring(arguments[index])
    end)
    output = output:gsub("\0LB\0", "{"):gsub("\0RB\0", "}")
    return output
end

function fmt.print(template, ...)
    io.write(fmt.format(template, ...))
end

function fmt.println(template, ...)
    print(fmt.format(template, ...))
end

function fmt.panic(message)
    error(message, 2)
end

function number.idiv(left, right)
    local quotient = left // right
    if (left % right ~= 0) and ((left < 0) ~= (right < 0)) then
        quotient = quotient + 1
    end
    return quotient
end

function number.irem(left, right)
    local remainder = left % right
    if remainder ~= 0 and ((remainder < 0) ~= (left < 0)) then
        remainder = remainder - right
    end
    return remainder
end

local Result = {}
Result.__index = Result

function result.ok(value)
    return setmetatable({ __rua_result = true, tag = "ok", value = value }, Result)
end

function result.err(value)
    return setmetatable({ __rua_result = true, tag = "err", value = value }, Result)
end

function Result:is_ok()
    return self.tag == "ok"
end

function Result:is_err()
    return self.tag == "err"
end

function Result:unwrap()
    if self.tag == "err" then error(self.value, 2) end
    return self.value
end

function Result:unwrap_or(default)
    if self.tag == "ok" then return self.value end
    return default
end

local Iter = {}
Iter.__index = Iter

function iter.new(next_value)
    return setmetatable({ next_value = next_value }, Iter)
end

function Iter:next()
    return self.next_value()
end

function Iter:map(transform)
    local source = self
    return iter.new(function()
        local value = source:next()
        if value == nil then return nil end
        return transform(value)
    end)
end

function Iter:filter(predicate)
    local source = self
    return iter.new(function()
        while true do
            local value = source:next()
            if value == nil or predicate(value) then return value end
        end
    end)
end

function Iter:filter_map(transform)
    local source = self
    return iter.new(function()
        while true do
            local value = source:next()
            if value == nil then return nil end
            local mapped = transform(value)
            if mapped ~= nil then return mapped end
        end
    end)
end

function Iter:enumerate()
    local source, index = self, 0
    return iter.new(function()
        local value = source:next()
        if value == nil then return nil end
        local pair = { [0] = index, value, n = 2 }
        index = index + 1
        return pair
    end)
end

function Iter:take(count)
    local source, remaining = self, math.max(0, count)
    return iter.new(function()
        if remaining == 0 then return nil end
        remaining = remaining - 1
        return source:next()
    end)
end

function Iter:skip(count)
    local source, remaining = self, math.max(0, count)
    return iter.new(function()
        while remaining > 0 do
            remaining = remaining - 1
            if source:next() == nil then return nil end
        end
        return source:next()
    end)
end

function Iter:collect()
    local values = { n = 0 }
    while true do
        local value = self:next()
        if value == nil then break end
        values[values.n] = value
        values.n = values.n + 1
    end
    return vec.from_table(values)
end

function Iter:fold(accumulator, reduce)
    while true do
        local value = self:next()
        if value == nil then return accumulator end
        accumulator = reduce(accumulator, value)
    end
end

function Iter:count()
    local count = 0
    while self:next() ~= nil do count = count + 1 end
    return count
end

function Iter:any(predicate)
    while true do
        local value = self:next()
        if value == nil then return false end
        if predicate(value) then return true end
    end
end

function Iter:all(predicate)
    while true do
        local value = self:next()
        if value == nil then return true end
        if not predicate(value) then return false end
    end
end

function Iter:find(predicate)
    while true do
        local value = self:next()
        if value == nil or predicate(value) then return value end
    end
end

function iter.range(start_value, end_value, inclusive)
    local current = start_value
    return iter.new(function()
        if (inclusive and current > end_value) or (not inclusive and current >= end_value) then
            return nil
        end
        local value = current
        current = current + 1
        return value
    end)
end

local Vec = {}
Vec.__index = Vec

function Vec:len()
    return self.n
end

function Vec:push(value)
    self[self.n] = value
    self.n = self.n + 1
end

function Vec:pop()
    if self.n == 0 then return nil end
    self.n = self.n - 1
    local value = self[self.n]
    self[self.n] = nil
    return value
end

function Vec:get(index)
    return self[index]
end

function Vec:set(index, value)
    self[index] = value
end

function Vec:iter()
    local vector, index = self, 0
    return iter.new(function()
        if index >= vector.n then return nil end
        local value = vector[index]
        index = index + 1
        return value
    end)
end

Vec.into_iter = Vec.iter

function vec.from_table(values)
    return setmetatable(values, Vec)
end

function vec.new()
    return vec.from_table({ n = 0 })
end

local Map = {}
Map.__index = Map

function Map:insert(key, value)
    local previous = self.values[key]
    if previous == nil then self.n = self.n + 1 end
    self.values[key] = value
    return previous
end

function Map:get(key)
    return self.values[key]
end

function Map:contains_key(key)
    return self.values[key] ~= nil
end

function Map:remove(key)
    local value = self.values[key]
    if value ~= nil then
        self.values[key] = nil
        self.n = self.n - 1
    end
    return value
end

function Map:len()
    return self.n
end

function map.new()
    return setmetatable({ values = {}, n = 0 }, Map)
end

function string_api.new()
    return ""
end

function string_api.to_string(value)
    return value
end

string_api.to_owned = string_api.to_string
string_api.clone = string_api.to_string

function string_api.len(value)
    return #value
end

function string_api.is_empty(value)
    return #value == 0
end

function string_api.to_uppercase(value)
    return value:upper()
end

function string_api.to_lowercase(value)
    return value:lower()
end

function string_api.trim(value)
    return (value:gsub("^%s*(.-)%s*$", "%1"))
end

function string_api.trim_start(value)
    return (value:gsub("^%s*", ""))
end

function string_api.trim_end(value)
    return (value:gsub("%s*$", ""))
end

function string_api.contains(value, needle)
    return value:find(needle, 1, true) ~= nil
end

function string_api.starts_with(value, prefix)
    return value:sub(1, #prefix) == prefix
end

function string_api.ends_with(value, suffix)
    return suffix == "" or value:sub(-#suffix) == suffix
end

function string_api.replace(value, from, to)
    if from == "" then return value end
    local output, index = {}, 1
    while true do
        local start = value:find(from, index, true)
        if start == nil then
            output[#output + 1] = value:sub(index)
            break
        end
        output[#output + 1] = value:sub(index, start - 1)
        output[#output + 1] = to
        index = start + #from
    end
    return table.concat(output)
end

function string_api.repeat_(value, count)
    if count <= 0 then return "" end
    return value:rep(count)
end

string_api["repeat"] = string_api.repeat_

function string_api.chars(value)
    local next_codepoint, state, control = utf8.codes(value)
    local done = false
    return iter.new(function()
        if done then return nil end
        local position, codepoint = next_codepoint(state, control)
        if position == nil then
            done = true
            return nil
        end
        control = position
        return utf8.char(codepoint)
    end)
end

function string_api.split(value, separator)
    local index, done = 1, false
    return iter.new(function()
        if done then return nil end
        if separator == "" then
            done = true
            return value
        end
        local start = value:find(separator, index, true)
        if start == nil then
            done = true
            return value:sub(index)
        end
        local part = value:sub(index, start - 1)
        index = start + #separator
        return part
    end)
end

return std
