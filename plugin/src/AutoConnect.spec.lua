return function()
	local AutoConnect = require(script.Parent.AutoConnect)
	local Config = require(script.Parent.Config)

	local Match = AutoConnect.Match

	local function info(fields)
		local base = {
			protocolVersion = Config.protocolVersion,
			projectName = "farm",
			sessionId = "a",
		}
		for key, value in fields or {} do
			base[key] = value
		end
		return base
	end

	describe("match", function()
		it("should match a project that lists the place", function()
			expect(AutoConnect.match(info({ placeId = 5 }), { placeId = 5 })).to.equal(Match.Place)
			expect(AutoConnect.match(info({ expectedPlaceIds = { 4, 5 } }), { placeId = 5 })).to.equal(Match.Place)
		end)

		it("should refuse servers for other places", function()
			expect(AutoConnect.match(info({ expectedPlaceIds = { 4 } }), { placeId = 5 })).to.equal(nil)
			expect(AutoConnect.match(info({ unexpectedPlaceIds = { 5 } }), { placeId = 5 })).to.equal(nil)
		end)

		it("should match the project last synced to the place", function()
			expect(AutoConnect.match(info(), { placeId = 5, priorProjectName = "farm" })).to.equal(Match.Known)
		end)

		it("should match a local file named after the project", function()
			expect(AutoConnect.match(info(), { placeId = 0, placeName = "farm" })).to.equal(Match.Known)
			expect(AutoConnect.match(info(), { placeId = 0, placeName = "other" })).to.equal(Match.Unknown)
		end)

		it("should refuse a project other than the one last synced", function()
			expect(AutoConnect.match(info(), { placeId = 5, priorProjectName = "fruit" })).to.equal(nil)
		end)

		it("should give unknown places the lowest match", function()
			expect(AutoConnect.match(info(), { placeId = 0 })).to.equal(Match.Unknown)
		end)

		it("should refuse servers with another protocol version", function()
			expect(AutoConnect.match(info({ protocolVersion = -1 }), { placeId = 5 })).to.equal(nil)
		end)
	end)

	describe("pick", function()
		it("should pick the best match", function()
			local best, unique = AutoConnect.pick({
				{ port = "1", match = Match.Unknown },
				{ port = "2", match = Match.Known },
			})
			expect(best.port).to.equal("2")
			expect(unique).to.equal(true)
		end)

		it("should not pick between equal matches", function()
			local _, unique = AutoConnect.pick({
				{ port = "1", match = Match.Unknown },
				{ port = "2", match = Match.Unknown },
			})
			expect(unique).to.equal(false)
		end)

		it("should prefer the Connect page port on a tie", function()
			local best, unique = AutoConnect.pick({
				{ port = "1", match = Match.Unknown },
				{ port = "2", match = Match.Unknown },
			}, "2")
			expect(best.port).to.equal("2")
			expect(unique).to.equal(true)
		end)
	end)

	describe("placeKey", function()
		it("should key local files by name", function()
			expect(AutoConnect.placeKey(0, "farm.rbxl")).to.equal("file:farm.rbxl")
			expect(AutoConnect.placeKey(12, "farm")).to.equal("12")
		end)
	end)
end
