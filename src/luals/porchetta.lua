---@meta

---@alias JsonValue nil|boolean|number|string|JsonObject|JsonArray|lightuserdata
---@alias JsonObject table<string, JsonValue>
---@alias JsonArray JsonValue[]

---Porchetta JSON helpers.
---
---JSON `null` decodes to `porchetta.json.null`; missing keys are Lua `nil`.
---Decoded JSON arrays and objects preserve their shape when encoded again,
---including empty arrays (`[]`) and empty objects (`{}`). Lua-created empty
---tables encode as JSON objects (`{}`).
---@class PorchettaJson
---@field null lightuserdata JSON null sentinel. Missing keys are Lua nil.
local PorchettaJson = {}

---Decode JSON content into Lua values.
---@param content string JSON content to decode.
---@return JsonValue value decoded JSON value.
function PorchettaJson.decode(content) end

---Encode a Lua value as compact JSON.
---Use `porchetta.json.null` to encode JSON null.
---@param value JsonValue value to encode.
---@return string json compact JSON content.
function PorchettaJson.encode(value) end

---Encode a Lua value as pretty-printed JSON.
---Use `porchetta.json.null` to encode JSON null.
---@param value JsonValue value to encode.
---@return string json pretty-printed JSON content.
function PorchettaJson.encode_pretty(value) end

---Porchetta manifest runtime helpers.
---@class Porchetta
---@field json PorchettaJson JSON helpers.
local Porchetta = {}

---Run an external command and return stdout.
---@param args string[] command and arguments; the first item is the executable.
---@param stdin? string optional stdin content.
---@return string stdout command stdout.
function Porchetta.system(args, stdin) end

---Return the current hostname.
---@return string hostname current hostname.
function Porchetta.hostname() end

---@type Porchetta
porchetta = Porchetta
