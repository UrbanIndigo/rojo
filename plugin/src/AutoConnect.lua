--[[
	Finds Rojo servers on this machine for the place Studio has open, so the
	plugin can fill in the port and, when the server was started with
	`rojo serve --autoconnect`, connect without the user pressing Connect.

	The plugin cannot read files or list processes, so it asks every localhost
	port in a small range for /api/rojo and picks the server that fits this
	place best.
]]

local Rojo = script:FindFirstAncestor("Rojo")
local Packages = Rojo.Packages

local Http = require(Packages.Http)
local Promise = require(Packages.Promise)

local Config = require(script.Parent.Config)

local AutoConnect = {}

-- Ports scanned from the default one. Projects usually pick a `servePort`
-- just above 34872, so this covers every port up to 34899.
AutoConnect.SCAN_PORTS = 28

-- How well a server fits the open place. Higher is better.
AutoConnect.Match = {
	-- The project lists this place in `servePlaceIds` or as its `placeId`,
	-- or `rojo build` built the place from it.
	Place = 3,
	-- This place was last synced with this project, or is a local file
	-- named after it.
	Known = 2,
	-- Nothing ties the server to this place, but nothing rules it out either.
	Unknown = 1,
}

local function contains(list, value): boolean
	return type(list) == "table" and table.find(list, value) ~= nil
end

-- place: { placeId: number, placeName: string?, builtProjectName: string?, priorProjectName: string? }
-- Returns a `Match` value, or nil when the server must not be used here.
function AutoConnect.match(serverInfo, place): number?
	if serverInfo.protocolVersion ~= Config.protocolVersion then
		return nil
	end

	if serverInfo.expectedPlaceIds ~= nil and not contains(serverInfo.expectedPlaceIds, place.placeId) then
		return nil
	end

	if contains(serverInfo.unexpectedPlaceIds, place.placeId) then
		return nil
	end

	if
		place.placeId ~= 0
		and (serverInfo.placeId == place.placeId or contains(serverInfo.expectedPlaceIds, place.placeId))
	then
		return AutoConnect.Match.Place
	end

	if place.builtProjectName == serverInfo.projectName then
		return AutoConnect.Match.Place
	end

	if place.priorProjectName == serverInfo.projectName then
		return AutoConnect.Match.Known
	end

	if place.placeId == 0 then
		-- Studio names a local file after the file, and build tools often
		-- name the file after the project
		local fileName = if place.placeName then string.gsub(place.placeName, "%.rbxlx?$", "") else nil
		if fileName == serverInfo.projectName then
			return AutoConnect.Match.Known
		end

		-- File names like game.rbxl are shared by many projects, so a file
		-- last synced with another project may still be this one's; its first
		-- sync is confirmed
		return AutoConnect.Match.Unknown
	end

	if place.priorProjectName ~= nil then
		-- A published place synced with a different project before is
		-- almost certainly some other project's place
		return nil
	end

	return AutoConnect.Match.Unknown
end

-- candidates: { { serverInfo, port: string, match: number } }
-- preferredPort is the port on the Connect page; it wins ties.
-- Returns the best candidate and whether it is the only one with that match.
function AutoConnect.pick(candidates, preferredPort: string?)
	local best, unique = nil, false
	for _, candidate in candidates do
		if best == nil or candidate.match > best.match then
			best, unique = candidate, true
		elseif candidate.match == best.match then
			if candidate.port == preferredPort then
				best = candidate
			elseif best.port ~= preferredPort then
				unique = false
			end
		end
	end
	return best, unique
end

-- The key auto-connect is turned off under for this place. Local files all
-- have place ID 0, so they go by the name Studio gives them, the file name.
function AutoConnect.placeKey(placeId: number, placeName: string): string
	return if placeId ~= 0 then tostring(placeId) else "file:" .. placeName
end

function AutoConnect.ports(): { string }
	local ports = {}
	local first = tonumber(Config.defaultPort)
	for offset = 0, AutoConnect.SCAN_PORTS - 1 do
		table.insert(ports, tostring(first + offset))
	end
	return ports
end

-- Asks each port for its server info. Resolves with { [port]: serverInfo }
-- for the ports that answered; closed ports fail fast and are left out.
function AutoConnect.scan(host: string, ports: { string })
	local found = {}
	local checks = {}
	for _, port in ports do
		local baseUrl = if string.find(host, "^https?://")
			then string.format("%s:%s", host, port)
			else string.format("http://%s:%s", host, port)

		table.insert(
			checks,
			Http.get(baseUrl .. "/api/rojo")
				:andThen(function(response)
					if response.code >= 400 then
						return
					end
					local ok, body = pcall(Http.Response.msgpack, response)
					if ok and type(body) == "table" and type(body.projectName) == "string" then
						found[port] = body
					end
				end)
				:catch(function()
					-- Usually nothing is listening on the port
				end)
		)
	end

	return Promise.all(checks):andThen(function()
		return found
	end)
end

return AutoConnect
