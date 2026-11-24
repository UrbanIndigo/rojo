local ChangeHistoryService = game:GetService("ChangeHistoryService")
local Players = game:GetService("Players")
local ServerStorage = game:GetService("ServerStorage")
local RunService = game:GetService("RunService")

local Rojo = script:FindFirstAncestor("Rojo")
local Plugin = Rojo.Plugin
local Packages = Rojo.Packages

local Roact = require(Packages.Roact)
local Log = require(Packages.Log)
local Promise = require(Packages.Promise)

local Assets = require(Plugin.Assets)
local Version = require(Plugin.Version)
local Config = require(Plugin.Config)
local Settings = require(Plugin.Settings)
local strict = require(Plugin.strict)
local Dictionary = require(Plugin.Dictionary)
local ServeSession = require(Plugin.ServeSession)
local ApiContext = require(Plugin.ApiContext)
local AutoConnect = require(Plugin.AutoConnect)
local PatchSet = require(Plugin.PatchSet)
local PatchTree = require(Plugin.PatchTree)
local preloadAssets = require(Plugin.preloadAssets)
local soundPlayer = require(Plugin.soundPlayer)
local ignorePlaceIds = require(Plugin.ignorePlaceIds)
local timeUtil = require(Plugin.timeUtil)
local Theme = require(script.Theme)

local Page = require(script.Page)
local Notifications = require(script.Components.Notifications)
local Tooltip = require(script.Components.Tooltip)
local StudioPluginAction = require(script.Components.Studio.StudioPluginAction)
local StudioToolbar = require(script.Components.Studio.StudioToolbar)
local StudioToggleButton = require(script.Components.Studio.StudioToggleButton)
local StudioPluginGui = require(script.Components.Studio.StudioPluginGui)
local StudioPluginContext = require(script.Components.Studio.StudioPluginContext)
local StatusPages = require(script.StatusPages)

local AppStatus = strict("AppStatus", {
	NotConnected = "NotConnected",
	Settings = "Settings",
	Connecting = "Connecting",
	Confirming = "Confirming",
	Connected = "Connected",
	Error = "Error",
})

local e = Roact.createElement

-- Seconds between checks for connection requests from agents using `rojo mcp`.
-- Agents wait on Studio, so this is short, and a request to a local server
-- that is not there fails right away.
local AGENT_CONNECT_POLL_INTERVAL = 2

-- Besides the host and port on the Connect page, the plugin checks this many
-- localhost ports from the default one for connection requests, so an agent
-- reaches Studio whichever port its project serves on. Must match
-- `AGENT_SCAN_PORTS` in src/mcp/serve.rs.
local AGENT_SCAN_PORTS = 10

-- Seconds between looks for a Rojo server for this place while not
-- connected. Each look asks `AutoConnect.SCAN_PORTS` localhost ports, so this
-- is kept well under Studio's HTTP request budget.
local AUTO_CONNECT_POLL_INTERVAL = 8

-- Agents can address a Studio window by place name. Studio names a place
-- opened from a file after the file, and a sync may rename the data model
-- later, so the name it had when the plugin loaded counts too.
local openedPlaceName = game.Name

-- `rojo build` writes the project's name and `servePort` into a built place's
-- Workspace, so the plugin knows where its server will be without a scan.
-- They are read once as the place opens.
local builtProjectName = workspace:GetAttribute("__Rojo_ProjectName")
local builtServePort = workspace:GetAttribute("__Rojo_ServePort")
local builtTreeFingerprint = workspace:GetAttribute("__Rojo_TreeFingerprint")
if type(builtTreeFingerprint) ~= "string" then
	builtTreeFingerprint = nil
end
if type(builtProjectName) ~= "string" then
	builtProjectName = nil
end
builtServePort = if type(builtServePort) == "number" then tostring(builtServePort) else nil

local function baseUrlOf(host: string, port: string): string
	return if string.find(host, "^https?://")
		then string.format("%s:%s", host, port)
		else string.format("http://%s:%s", host, port)
end

local function isForThisPlace(request): boolean
	if request.placeId ~= nil and request.placeId ~= game.PlaceId then
		return false
	end

	if request.placeName ~= nil and request.placeName ~= openedPlaceName and request.placeName ~= game.Name then
		return false
	end

	return true
end

local App = Roact.Component:extend("App")

function App:init()
	preloadAssets()

	local priorSyncInfo = self:getPriorSyncInfo()
	self.host, self.setHost = Roact.createBinding(priorSyncInfo.host or "")
	-- A place built by Rojo says which port its project serves on, which is
	-- newer than any port remembered for the place
	local initialPort = priorSyncInfo.port or ""
	if builtServePort ~= nil then
		initialPort = if builtServePort == Config.defaultPort then "" else builtServePort
	end
	self.port, self.setPort = Roact.createBinding(initialPort)
	-- The port auto-connect last put on the Connect page. A port the user
	-- typed is left alone, but a filled or remembered one follows the server.
	self.autoFilledPort = initialPort
	-- Server session IDs that were connected to or auto-connected once. Each
	-- server is auto-connected at most once, so disconnecting stays
	-- disconnected until `rojo serve` restarts.
	self.autoConnectSeen = {}

	self.confirmationBindable = Instance.new("BindableEvent")
	self.confirmationEvent = self.confirmationBindable.Event
	self.knownProjects = {}
	self.handledAgentRequests = {}
	self.notifId = 0

	self.waypointConnection = ChangeHistoryService.OnUndo:Connect(function(action: string)
		if not string.find(action, "^Rojo: Patch") then
			return
		end

		local undoConnection, redoConnection = nil, nil
		local function cleanup()
			undoConnection:Disconnect()
			redoConnection:Disconnect()
		end

		Log.warn(
			string.format(
				"You've undone '%s'.\nIf this was not intended, please Redo in the topbar or with Ctrl/⌘+Y.",
				action
			)
		)
		local dismissNotif = self:addNotification({
			text = string.format("You've undone '%s'.\nIf this was not intended, please restore.", action),
			timeout = 10,
			onClose = function()
				cleanup()
			end,
			actions = {
				Restore = {
					text = "Restore",
					style = "Solid",
					layoutOrder = 1,
					onClick = function()
						ChangeHistoryService:Redo()
					end,
				},
				Dismiss = {
					text = "Dismiss",
					style = "Bordered",
					layoutOrder = 2,
				},
			},
		})

		undoConnection = ChangeHistoryService.OnUndo:Once(function()
			-- Our notif is now out of date- redoing will not restore the patch
			-- since we've undone even further. Dismiss the notif.
			cleanup()
			dismissNotif()
		end)
		redoConnection = ChangeHistoryService.OnRedo:Once(function(redoneAction: string)
			if redoneAction == action then
				-- The user has restored the patch, so we can dismiss the notif
				cleanup()
				dismissNotif()
			end
		end)
	end)

	self.disconnectUpdatesCheckChanged = Settings:onChanged("checkForUpdates", function()
		self:checkForUpdates()
	end)
	self.disconnectPrereleasesCheckChanged = Settings:onChanged("checkForPrereleases", function()
		self:checkForUpdates()
	end)

	self:setState({
		appStatus = AppStatus.NotConnected,
		guiEnabled = false,
		confirmData = {},
		patchData = {
			patch = PatchSet.newEmpty(),
			unapplied = PatchSet.newEmpty(),
			timestamp = os.time(),
		},
		notifications = {},
		toolbarIcon = Assets.Images.PluginButton,
		autoConnectHere = self:isAutoConnectOnHere(),
		autoConnectStatus = nil,
	})

	self.disconnectAutoConnectChanged = Settings:onChanged("autoConnect", function()
		self:setState({ autoConnectHere = self:isAutoConnectOnHere() })
	end)
	self.disconnectAutoConnectPlacesChanged = Settings:onChanged("autoConnectDisabledPlaces", function()
		self:setState({ autoConnectHere = self:isAutoConnectOnHere() })
	end)

	if RunService:IsEdit() then
		self:checkForUpdates()

		self:startSyncReminderPolling()
		self.disconnectSyncReminderPollingChanged = Settings:onChanged("syncReminderPolling", function(enabled)
			if enabled then
				self:startSyncReminderPolling()
			else
				self:stopSyncReminderPolling()
			end
		end)

		self:startAgentConnectPolling()
		self.disconnectAgentConnectionsChanged = Settings:onChanged("agentConnections", function(enabled)
			if enabled then
				self:startAgentConnectPolling()
			else
				self:stopAgentConnectPolling()
			end
		end)

		self:tryAutoReconnect():andThen(function(didReconnect)
			if not didReconnect then
				self:checkSyncReminder()
			end
		end)

		self:startAutoConnectPolling()
	end

	if self:isAutoConnectPlaytestServerAvailable() then
		self:useRunningConnectionInfo()
		self:startSession()
	end
	self.autoConnectPlaytestServerListener = Settings:onChanged("autoConnectPlaytestServer", function(enabled)
		if enabled then
			if self:isAutoConnectPlaytestServerWriteable() and self.serveSession ~= nil then
				-- Write the existing session
				local baseUrl = self.serveSession.__apiContext.__baseUrl
				self:setRunningConnectionInfo(baseUrl)
			end
		else
			self:clearRunningConnectionInfo()
		end
	end)
end

function App:willUnmount()
	self:endSession()

	self.waypointConnection:Disconnect()
	self.confirmationBindable:Destroy()

	self.disconnectUpdatesCheckChanged()
	self.disconnectPrereleasesCheckChanged()
	if self.disconnectSyncReminderPollingChanged then
		self.disconnectSyncReminderPollingChanged()
	end

	self:stopSyncReminderPolling()

	if self.disconnectAgentConnectionsChanged then
		self.disconnectAgentConnectionsChanged()
	end
	self:stopAgentConnectPolling()

	self.disconnectAutoConnectChanged()
	self.disconnectAutoConnectPlacesChanged()
	self:stopAutoConnectPolling()

	self.autoConnectPlaytestServerListener()
	self:clearRunningConnectionInfo()
end

function App:addNotification(notif: {
	text: string,
	isFullscreen: boolean?,
	timeout: number?,
	actions: { [string]: { text: string, style: string, layoutOrder: number, onClick: (any) -> ()? } }?,
	onClose: (any) -> ()?,
})
	if not Settings:get("showNotifications") then
		return
	end

	self.notifId += 1
	local id = self.notifId

	self:setState(function(prevState)
		local notifications = table.clone(prevState.notifications)
		notifications[id] = Dictionary.merge({
			timeout = notif.timeout or 5,
			isFullscreen = notif.isFullscreen or false,
		}, notif)

		return {
			notifications = notifications,
		}
	end)

	return function()
		self:closeNotification(id)
	end
end

function App:closeNotification(id: number)
	if not self.state.notifications[id] then
		return
	end

	self:setState(function(prevState)
		local notifications = table.clone(prevState.notifications)
		notifications[id] = nil

		return {
			notifications = notifications,
		}
	end)
end

function App:checkForUpdates()
	local updateMessage = Version.getUpdateMessage()

	if updateMessage then
		self:addNotification({
			text = updateMessage,
			timeout = 500,
			actions = {
				Dismiss = {
					text = "Dismiss",
					style = "Bordered",
					layoutOrder = 2,
				},
			},
		})
	end
end

function App:getPriorSyncInfo(): { host: string?, port: string?, projectName: string?, timestamp: number? }
	local priorSyncInfos = Settings:get("priorEndpoints")
	if not priorSyncInfos then
		return {}
	end

	local id = tostring(game.PlaceId)
	if ignorePlaceIds[id] then
		return {}
	end

	return priorSyncInfos[id] or {}
end

function App:setPriorSyncInfo(host: string, port: string, projectName: string)
	local priorSyncInfos = Settings:get("priorEndpoints")
	if not priorSyncInfos then
		priorSyncInfos = {}
	end

	local now = os.time()

	-- Clear any stale saves to avoid disc bloat
	for placeId, syncInfo in priorSyncInfos do
		if now - (syncInfo.timestamp or now) > 12_960_000 then
			priorSyncInfos[placeId] = nil
			Log.trace("Cleared stale saved endpoint for {}", placeId)
		end
	end

	local id = tostring(game.PlaceId)
	if ignorePlaceIds[id] then
		return
	end

	priorSyncInfos[id] = {
		host = if host ~= Config.defaultHost then host else nil,
		port = if port ~= Config.defaultPort then port else nil,
		projectName = projectName,
		timestamp = now,
	}
	Log.trace("Saved last used endpoint for {}", game.PlaceId)

	Settings:set("priorEndpoints", priorSyncInfos)
end

function App:forgetPriorSyncInfo()
	local priorSyncInfos = Settings:get("priorEndpoints")
	if not priorSyncInfos then
		priorSyncInfos = {}
	end

	local id = tostring(game.PlaceId)
	priorSyncInfos[id] = nil
	Log.trace("Erased last used endpoint for {}", game.PlaceId)

	Settings:set("priorEndpoints", priorSyncInfos)
end

function App:getHostAndPort()
	local host = self.host:getValue()
	local port = self.port:getValue()

	return if #host > 0 then host else Config.defaultHost, if #port > 0 then port else Config.defaultPort
end

function App:isSyncLockAvailable()
	if #Players:GetPlayers() == 0 then
		-- Team Create is not active, so no one can be holding the lock
		return true
	end

	local lock = ServerStorage:FindFirstChild("__Rojo_SessionLock")
	if not lock then
		-- No lock is made yet, so it is available
		return true
	end

	if lock.Value and lock.Value ~= Players.LocalPlayer and lock.Value.Parent then
		-- Someone else is holding the lock
		return false, lock.Value
	end

	-- The lock exists, but is not claimed
	return true
end

function App:claimSyncLock()
	if #Players:GetPlayers() == 0 then
		Log.trace("Skipping sync lock because this isn't in Team Create")
		return true
	end

	local isAvailable, priorOwner = self:isSyncLockAvailable()
	if not isAvailable then
		Log.trace("Skipping sync lock because it is already claimed")
		return false, priorOwner
	end

	local lock = ServerStorage:FindFirstChild("__Rojo_SessionLock")
	if not lock then
		lock = Instance.new("ObjectValue")
		lock.Name = "__Rojo_SessionLock"
		lock.Archivable = false
		lock.Value = Players.LocalPlayer
		lock.Parent = ServerStorage
		Log.trace("Created and claimed sync lock")
		return true
	end

	lock.Value = Players.LocalPlayer
	Log.trace("Claimed existing sync lock")
	return true
end

function App:releaseSyncLock()
	local lock = ServerStorage:FindFirstChild("__Rojo_SessionLock")
	if not lock then
		Log.trace("No sync lock found, assumed released")
		return
	end

	if lock.Value == Players.LocalPlayer then
		lock.Value = nil
		Log.trace("Released sync lock")
		return
	end

	Log.trace("Could not relase sync lock because it is owned by {}", lock.Value)
end

function App:findActiveServer()
	local host, port = self:getHostAndPort()
	local baseUrl = if string.find(host, "^https?://")
		then string.format("%s:%s", host, port)
		else string.format("http://%s:%s", host, port)

	Log.trace("Checking for active sync server at {}", baseUrl)

	local apiContext = ApiContext.new(baseUrl)
	return apiContext:connect():andThen(function(serverInfo)
		apiContext:disconnect()
		return serverInfo, host, port
	end)
end

function App:tryAutoReconnect()
	if not Settings:get("autoReconnect") then
		return Promise.resolve(false)
	end

	local priorSyncInfo = self:getPriorSyncInfo()
	if not priorSyncInfo.projectName then
		Log.trace("No prior sync info found, skipping auto-reconnect")
		return Promise.resolve(false)
	end

	return self:findActiveServer()
		:andThen(function(serverInfo)
			-- change
			if serverInfo.projectName == priorSyncInfo.projectName then
				Log.trace("Auto-reconnect found matching server, reconnecting...")
				self:addNotification({
					text = `Auto-reconnect discovered project '{serverInfo.projectName}'...`,
				})
				self:startSession()
				return true
			end
			Log.trace("Auto-reconnect found different server, not reconnecting")
			return false
		end)
		:catch(function()
			Log.trace("Auto-reconnect did not find a server, not reconnecting")
			return false
		end)
end

function App:checkSyncReminder()
	local syncReminderMode = Settings:get("syncReminderMode")
	if syncReminderMode == "None" then
		return
	end

	if self.serveSession ~= nil or not self:isSyncLockAvailable() then
		-- Already syncing or cannot sync, no reason to remind
		return
	end

	local priorSyncInfo = self:getPriorSyncInfo()

	self:findActiveServer()
		:andThen(function(serverInfo, host, port)
			if self.serveSession ~= nil or self:willAutoConnectTo(serverInfo) then
				-- Auto-connect handles this server, and the reminder would
				-- otherwise show up after the session has connected
				return
			end
			self:sendSyncReminder(
				`Project '{serverInfo.projectName}' is serving at {host}:{port}.\nWould you like to connect?`,
				{ "Connect", "Dismiss" }
			)
		end)
		:catch(function()
			if priorSyncInfo.timestamp and priorSyncInfo.projectName then
				-- We didn't find an active server,
				-- but this place has a prior sync
				-- so we should remind the user to serve

				local timeSinceSync = timeUtil.elapsedToText(os.time() - priorSyncInfo.timestamp)
				self:sendSyncReminder(
					`You synced project '{priorSyncInfo.projectName}' to this place {timeSinceSync}.\nDid you mean to run 'rojo serve' and then connect?`,
					{ "Connect", "Forget", "Dismiss" }
				)
			end
		end)
end

function App:startSyncReminderPolling()
	if
		self.syncReminderPollingThread ~= nil
		or Settings:get("syncReminderMode") == "None"
		or not Settings:get("syncReminderPolling")
	then
		return
	end

	Log.trace("Starting sync reminder polling thread")
	self.syncReminderPollingThread = task.spawn(function()
		while task.wait(30) do
			if self.syncReminderPollingThread == nil then
				-- The polling thread was stopped, so exit
				return
			end
			if self.dismissSyncReminder then
				-- There is already a sync reminder being shown
				task.wait(5)
				continue
			end
			self:checkSyncReminder()
		end
	end)
end

function App:stopSyncReminderPolling()
	if self.syncReminderPollingThread then
		Log.trace("Stopping sync reminder polling thread")
		task.cancel(self.syncReminderPollingThread)
		self.syncReminderPollingThread = nil
	end
end

function App:sendSyncReminder(message: string, shownActions: { string })
	local syncReminderMode = Settings:get("syncReminderMode")
	if syncReminderMode == "None" or self.serveSession ~= nil then
		return
	end

	local connectIndex = table.find(shownActions, "Connect")
	local forgetIndex = table.find(shownActions, "Forget")
	local dismissIndex = table.find(shownActions, "Dismiss")

	self.dismissSyncReminder = self:addNotification({
		text = message,
		timeout = 120,
		isFullscreen = Settings:get("syncReminderMode") == "Fullscreen",
		onClose = function()
			self.dismissSyncReminder = nil
		end,
		actions = {
			Connect = if connectIndex
				then {
					text = "Connect",
					style = "Solid",
					layoutOrder = connectIndex,
					onClick = function()
						self:startSession()
					end,
				}
				else nil,
			Forget = if forgetIndex
				then {
					text = "Forget",
					style = "Bordered",
					layoutOrder = forgetIndex,
					onClick = function()
						-- The user doesn't want to be reminded again about this sync
						self:forgetPriorSyncInfo()
					end,
				}
				else nil,
			Dismiss = if dismissIndex
				then {
					text = "Dismiss",
					style = "Bordered",
					layoutOrder = dismissIndex,
					onClick = function()
						-- If the user dismisses the reminder,
						-- then we don't need to remind them again
						self:stopSyncReminderPolling()
					end,
				}
				else nil,
		},
	})
end

function App:startAgentConnectPolling()
	if self.agentConnectPollingThread ~= nil or not Settings:get("agentConnections") then
		return
	end

	Log.trace("Starting agent connection polling thread")
	self.agentConnectPollingThread = task.spawn(function()
		while task.wait(AGENT_CONNECT_POLL_INTERVAL) do
			if self.agentConnectPollingThread == nil then
				-- The polling thread was stopped, so exit
				return
			end
			if self.serveSession == nil and not self.checkingAgentConnectRequest then
				self:checkAgentConnectRequest()
			end
		end
	end)
end

function App:stopAgentConnectPolling()
	if self.agentConnectPollingThread then
		Log.trace("Stopping agent connection polling thread")
		task.cancel(self.agentConnectPollingThread)
		self.agentConnectPollingThread = nil
	end
end

-- The host and port on the Connect page, then localhost ports from the
-- default one.
function App:getAgentEndpoints(): { { host: string, port: string } }
	local host, port = self:getHostAndPort()
	local endpoints = { { host = host, port = port } }
	local seen = { [host .. ":" .. port] = true }

	local first = tonumber(Config.defaultPort)
	for offset = 0, AGENT_SCAN_PORTS - 1 do
		local scanned = tostring(first + offset)
		local key = Config.defaultHost .. ":" .. scanned
		if not seen[key] then
			seen[key] = true
			table.insert(endpoints, { host = Config.defaultHost, port = scanned })
		end
	end

	return endpoints
end

-- Whether a request is new and meant for this place. Each request is looked
-- at once, so declining the initial sync does not lead straight into another
-- connection attempt.
function App:takesAgentRequest(status, request): boolean
	-- Request IDs start over with every server, so the session ID is needed
	-- to tell a new request from one that was already handled
	local requestKey = string.format("%s:%s", tostring(status.sessionId), tostring(request.id))
	if self.handledAgentRequests[requestKey] then
		return false
	end
	self.handledAgentRequests[requestKey] = true

	if not isForThisPlace(request) then
		Log.trace("Ignoring agent connection request {} for another place", requestKey)
		return false
	end

	return true
end

function App:answerAgentRequest(endpoint, status, request)
	return ApiContext.new(baseUrlOf(endpoint.host, endpoint.port))
		:claimAgentRequest(request.id)
		:andThen(function(claimed)
			if not claimed or self.serveSession ~= nil then
				return
			end

			Log.info("An agent asked to connect to project '{}'", status.projectName)
			self:addNotification({
				text = string.format("An agent asked to connect to project '%s'.", tostring(status.projectName)),
				timeout = 10,
			})
			self.setHost(endpoint.host)
			self.setPort(endpoint.port)
			-- Show the widget, since the initial sync may need to be confirmed
			self:setState({
				guiEnabled = true,
			})
			self:startSession()
		end)
end

function App:checkAgentConnectRequest()
	self.checkingAgentConnectRequest = true

	local endpoints = self:getAgentEndpoints()
	local statuses = {}
	local checks = {}
	for index, endpoint in endpoints do
		checks[index] = ApiContext.new(baseUrlOf(endpoint.host, endpoint.port))
			:getAgentStatus()
			:andThen(function(status)
				statuses[index] = status
			end)
			:catch(function(err)
				-- Most of the time no server is running there, so this is expected
				Log.trace("Could not check {}:{} for agent connection requests: {}", endpoint.host, endpoint.port, err)
			end)
	end

	Promise.all(checks)
		:andThen(function()
			for index, endpoint in endpoints do
				local status = statuses[index]
				local request = status and status.connectRequest
				if request ~= nil and self:takesAgentRequest(status, request) then
					return self:answerAgentRequest(endpoint, status, request)
				end
			end
			return nil
		end)
		:catch(function(err)
			Log.trace("Could not answer an agent connection request: {}", err)
		end)
		:finally(function()
			self.checkingAgentConnectRequest = false
		end)
end

function App:getAutoConnectPlaceKey(): string
	return AutoConnect.placeKey(game.PlaceId, openedPlaceName)
end

-- The project last synced to this place. Unlike `priorEndpoints`, this is
-- kept for local files too, keyed by file name, so they need confirming once.
function App:getAutoConnectProject(): string?
	local projects = Settings:get("autoConnectProjects") or {}
	return projects[self:getAutoConnectPlaceKey()]
end

function App:setAutoConnectProject(projectName: string)
	if self:getAutoConnectProject() == projectName then
		return
	end
	local projects = table.clone(Settings:get("autoConnectProjects") or {})
	projects[self:getAutoConnectPlaceKey()] = projectName
	Settings:set("autoConnectProjects", projects)
end

-- Whether auto-connect will connect to this server by itself, so asking the
-- user to connect would only get in its way.
function App:willAutoConnectTo(serverInfo): boolean
	return serverInfo.autoConnect == true
		and self:isAutoConnectOnHere()
		and not self.autoConnectSeen[serverInfo.sessionId]
		and AutoConnect.match(serverInfo, {
				placeId = game.PlaceId,
				placeName = openedPlaceName,
				builtProjectName = builtProjectName,
				priorProjectName = self:getAutoConnectProject() or self:getPriorSyncInfo().projectName,
			})
			~= nil
end

function App:isAutoConnectOnHere(): boolean
	local disabled = Settings:get("autoConnectDisabledPlaces") or {}
	return Settings:get("autoConnect") and not disabled[self:getAutoConnectPlaceKey()]
end

function App:setAutoConnectHere(enabled: boolean)
	if enabled and not Settings:get("autoConnect") then
		-- Turning it on for one place should not need a trip to Settings
		Settings:set("autoConnect", true)
	end

	local disabled = table.clone(Settings:get("autoConnectDisabledPlaces") or {})
	disabled[self:getAutoConnectPlaceKey()] = if enabled then nil else true
	Settings:set("autoConnectDisabledPlaces", disabled)

	if enabled then
		-- Let a server that was skipped while it was off connect now
		table.clear(self.autoConnectSeen)
		self:checkAutoConnect()
	end
end

function App:startAutoConnectPolling()
	if self.autoConnectPollingThread ~= nil then
		return
	end

	Log.trace("Starting auto-connect polling thread")
	-- task.spawn runs the function before its thread is assigned, so the loop
	-- must not check `autoConnectPollingThread`; stopping cancels the thread.
	self.autoConnectPollingThread = task.spawn(function()
		while true do
			-- Look right away, so a server that is already running connects
			-- as the place opens
			if self.serveSession == nil and not self.checkingAutoConnect then
				self:checkAutoConnect()
			end
			task.wait(AUTO_CONNECT_POLL_INTERVAL)
		end
	end)
end

function App:stopAutoConnectPolling()
	if self.autoConnectPollingThread then
		Log.trace("Stopping auto-connect polling thread")
		task.cancel(self.autoConnectPollingThread)
		self.autoConnectPollingThread = nil
	end
end

-- Looks for a server for this place. It fills in the Connect page's port
-- with the one the server is on, and connects when the server was started
-- with `rojo serve --autoconnect`.
function App:checkAutoConnect()
	if self.serveSession ~= nil or self.checkingAutoConnect then
		return
	end
	self.checkingAutoConnect = true

	local host, port = self:getHostAndPort()
	local priorSyncInfo = self:getPriorSyncInfo()

	local place = {
		placeId = game.PlaceId,
		placeName = openedPlaceName,
		builtProjectName = builtProjectName,
		priorProjectName = self:getAutoConnectProject() or priorSyncInfo.projectName,
	}

	local function candidatesOf(found)
		local candidates = {}
		for foundPort, serverInfo in found do
			local match = AutoConnect.match(serverInfo, place)
			if match ~= nil then
				table.insert(candidates, { serverInfo = serverInfo, port = foundPort, match = match })
			end
		end
		return candidates
	end

	-- The ports the place points at are asked first: the Connect page's, the
	-- one the built place names and the one last used here. Only when none
	-- of them has a server for this place is the rest of the range scanned,
	-- and only on this machine; a remote host gets just its own port.
	local likelyPorts = {}
	for _, likely in { port, builtServePort, priorSyncInfo.port } do
		if likely ~= nil and likely ~= "" and not table.find(likelyPorts, likely) then
			table.insert(likelyPorts, likely)
		end
	end
	local otherPorts = {}
	if host == Config.defaultHost or host == "127.0.0.1" then
		for _, scanned in AutoConnect.ports() do
			if not table.find(likelyPorts, scanned) then
				table.insert(otherPorts, scanned)
			end
		end
	end

	AutoConnect.scan(host, likelyPorts)
		:andThen(function(found)
			local candidates = candidatesOf(found)
			local best = AutoConnect.pick(candidates, port)
			-- Ports picked by `rojo serve` change between runs, so a weaker
			-- match on a remembered port may be another project's server
			-- while this place's is elsewhere in the range
			if (best ~= nil and best.match == AutoConnect.Match.Place) or #otherPorts == 0 then
				return candidates
			end
			return AutoConnect.scan(host, otherPorts):andThen(function(more)
				for _, candidate in candidatesOf(more) do
					table.insert(candidates, candidate)
				end
				return candidates
			end)
		end)
		:andThen(function(candidates)
			local best, unique = AutoConnect.pick(candidates, port)
			if best == nil or not unique then
				self:setState({
					autoConnectStatus = if best ~= nil then "Several servers found; pick a port" else Roact.None,
				})
				return
			end

			local serverInfo = best.serverInfo
			local currentPort = self.port:getValue()
			if currentPort == "" or currentPort == self.autoFilledPort then
				local fillPort = if best.port == Config.defaultPort then "" else best.port
				if currentPort ~= fillPort then
					Log.trace("Filling in port {} for project '{}'", best.port, serverInfo.projectName)
					self.setPort(fillPort)
				end
				self.autoFilledPort = fillPort
			end

			local canAutoConnect = serverInfo.autoConnect == true
			self:setState({
				autoConnectStatus = if canAutoConnect
					then string.format("Found '%s' on %s", serverInfo.projectName, best.port)
					else string.format("Found '%s' on %s (no --autoconnect)", serverInfo.projectName, best.port),
			})

			if
				not canAutoConnect
				or not self:isAutoConnectOnHere()
				or self.autoConnectSeen[serverInfo.sessionId]
				or self.serveSession ~= nil
				or not self:isSyncLockAvailable()
			then
				return
			end
			self.autoConnectSeen[serverInfo.sessionId] = true

			Log.info("Auto-connecting to project '{}' on {}:{}", serverInfo.projectName, host, best.port)
			self.setPort(best.port)
			self.autoFilledPort = best.port
			self.pendingAutoConnect = {
				match = best.match,
				projectName = serverInfo.projectName,
			}
			self:startSession()
		end)
		:catch(function(err)
			Log.trace("Auto-connect check failed: {}", err)
		end)
		:finally(function()
			self.checkingAutoConnect = false
		end)
end

function App:isAutoConnectPlaytestServerAvailable()
	return RunService:IsRunning()
		and RunService:IsStudio()
		and RunService:IsServer()
		and Settings:get("autoConnectPlaytestServer")
		and workspace:GetAttribute("__Rojo_ConnectionUrl")
end

function App:isAutoConnectPlaytestServerWriteable()
	return RunService:IsEdit() and Settings:get("autoConnectPlaytestServer")
end

function App:setRunningConnectionInfo(baseUrl: string)
	if not self:isAutoConnectPlaytestServerWriteable() then
		return
	end

	Log.trace("Setting connection info for play solo auto-connect")
	workspace:SetAttribute("__Rojo_ConnectionUrl", baseUrl)
end

function App:clearRunningConnectionInfo()
	if not RunService:IsEdit() then
		-- Only write connection info from edit mode
		return
	end

	Log.trace("Clearing connection info for play solo auto-connect")
	workspace:SetAttribute("__Rojo_ConnectionUrl", nil)
end

function App:useRunningConnectionInfo()
	local connectionInfo = workspace:GetAttribute("__Rojo_ConnectionUrl")
	if not connectionInfo then
		return
	end

	Log.trace("Using connection info for play solo auto-connect")
	local host, port = string.match(connectionInfo, "^(.+):(.-)$")

	self.setHost(host)
	self.setPort(port)
end

-- The fingerprint of the tree the open place was built from, while the place
-- can still be trusted to be exactly that build: nothing has been synced to
-- it in this Studio session, nothing can be undone (so it hasn't been
-- edited), and no one else is in it.
function App:getFastSyncFingerprint(): string?
	if builtTreeFingerprint == nil or self.syncedThisStudioSession or #Players:GetPlayers() > 0 then
		return nil
	end

	local ok, canUndo = pcall(function()
		return ChangeHistoryService:GetCanUndo()
	end)
	if not ok or canUndo then
		return nil
	end

	return builtTreeFingerprint
end

function App:startSession()
	if self.serveSession ~= nil then
		-- A session is already active
		-- Without this guard double clicking connect starts 2 serve sessions and leaks the first and crashes the plugn
		Log.trace("Ignoring startSession because a session is already active")
		return
	end

	local claimedLock, priorOwner = self:claimSyncLock()
	if not claimedLock then
		local msg = string.format("Could not sync because user '%s' is already syncing", tostring(priorOwner))

		Log.warn(msg)
		self:addNotification({
			text = msg,
			timeout = 10,
		})
		self:setState({
			appStatus = AppStatus.Error,
			errorMessage = msg,
			toolbarIcon = Assets.Images.PluginButtonWarning,
		})

		return
	end

	local host, port = self:getHostAndPort()

	local baseUrl = if string.find(host, "^https?://")
		then string.format("%s:%s", host, port)
		else string.format("http://%s:%s", host, port)
	local apiContext = ApiContext.new(baseUrl)

	local serveSession = ServeSession.new({
		apiContext = apiContext,
		twoWaySync = Settings:get("twoWaySync"),
		fastSyncFingerprint = self:getFastSyncFingerprint(),
	})
	-- Assign before start() so the guard above rejects simultaneous calls
	self.serveSession = serveSession

	serveSession:setUpdateLoadingTextCallback(function(text: string)
		self:setState({
			connectingText = text,
		})
	end)

	self.cleanupPrecommit = serveSession:hookPrecommit(function(patch, instanceMap)
		-- Build new tree for patch
		self:setState({
			patchTree = PatchTree.build(patch, instanceMap, { "Property", "Old", "New" }),
		})
	end)
	self.cleanupPostcommit = serveSession:hookPostcommit(function(patch, instanceMap, unappliedPatch)
		local now = DateTime.now().UnixTimestamp
		self:setState(function(prevState)
			local oldPatchData = prevState.patchData
			local newPatchData = {
				patch = patch,
				unapplied = unappliedPatch,
				timestamp = now,
			}

			if PatchSet.isEmpty(patch) then
				-- Keep existing patch info, but use new timestamp
				newPatchData.patch = oldPatchData.patch
				newPatchData.unapplied = oldPatchData.unapplied
			elseif now - oldPatchData.timestamp < 2 then
				-- Patches that apply in the same second are combined for human clarity
				newPatchData.patch = PatchSet.assign(PatchSet.newEmpty(), oldPatchData.patch, patch)
				newPatchData.unapplied = PatchSet.assign(PatchSet.newEmpty(), oldPatchData.unapplied, unappliedPatch)
			end

			return {
				patchTree = PatchTree.updateMetadata(prevState.patchTree, patch, instanceMap, unappliedPatch),
				patchData = newPatchData,
			}
		end)
	end)

	serveSession:onStatusChanged(function(status, details)
		if status == ServeSession.Status.Connecting then
			if self.dismissSyncReminder then
				self.dismissSyncReminder()
				self.dismissSyncReminder = nil
			end

			self:setState({
				appStatus = AppStatus.Connecting,
				toolbarIcon = Assets.Images.PluginButton,
			})
			if self.pendingAutoConnect == nil then
				-- Auto-connect says so once it has connected instead
				self:addNotification({
					text = "Connecting to session...",
				})
			end
		elseif status == ServeSession.Status.Connected then
			self.syncedThisStudioSession = true
			self.knownProjects[details] = true
			self:setPriorSyncInfo(host, port, details)
			self:setRunningConnectionInfo(baseUrl)
			if apiContext.__sessionId ~= nil then
				self.autoConnectSeen[apiContext.__sessionId] = true
			end
			if not ignorePlaceIds[tostring(game.PlaceId)] or game.PlaceId == 0 then
				self:setAutoConnectProject(details)
			end

			if self.dismissSyncReminder then
				self.dismissSyncReminder()
				self.dismissSyncReminder = nil
			end

			local address = ("%s:%s"):format(host, port)
			local autoConnected = self.pendingAutoConnect ~= nil
			self:setState({
				appStatus = AppStatus.Connected,
				projectName = details,
				address = if autoConnected then address .. "  ·  auto-connected" else address,
				toolbarIcon = Assets.Images.PluginButtonConnected,
			})
			if autoConnected then
				self:addNotification({
					text = string.format("Auto-connected to '%s' on %s.", details, port),
					timeout = 4,
					actions = {
						TurnOff = {
							text = "Turn off here",
							style = "Bordered",
							layoutOrder = 1,
							onClick = function()
								self:setAutoConnectHere(false)
								self:endSession()
							end,
						},
					},
				})
			else
				self:addNotification({
					text = string.format("Connected to session '%s' at %s.", details, address),
				})
			end
		elseif status == ServeSession.Status.Disconnected then
			self.serveSession = nil
			self.pendingAutoConnect = nil
			self:releaseSyncLock()
			self:clearRunningConnectionInfo()
			self:setState({
				patchData = {
					patch = PatchSet.newEmpty(),
					unapplied = PatchSet.newEmpty(),
					timestamp = os.time(),
				},
			})

			-- Details being present indicates that this
			-- disconnection was from an error.
			if details ~= nil then
				Log.warn("Disconnected from an error: {}", details)

				self:setState({
					appStatus = AppStatus.Error,
					errorMessage = tostring(details),
					toolbarIcon = Assets.Images.PluginButtonWarning,
				})
				self:addNotification({
					text = tostring(details),
					timeout = 10,
				})
			else
				self:setState({
					appStatus = AppStatus.NotConnected,
					toolbarIcon = Assets.Images.PluginButton,
				})
				self:addNotification({
					text = "Disconnected from session.",
					timeout = 10,
				})
			end
		end
	end)

	serveSession:setConfirmCallback(function(instanceMap, patch, serverInfo)
		if PatchSet.isEmpty(patch) then
			Log.trace("Accepting patch without confirmation because it is empty")
			return "Accept"
		end

		-- Play solo auto-connect does not require confirmation
		if self:isAutoConnectPlaytestServerAvailable() then
			Log.trace("Accepting patch without confirmation because play solo auto-connect is enabled")
			return "Accept"
		end

		local confirmationBehavior = Settings:get("confirmationBehavior")
		local autoConnect = self.pendingAutoConnect
		if autoConnect ~= nil then
			if autoConnect.match == AutoConnect.Match.Unknown then
				-- Nothing ties this server to the place yet, so the first
				-- sync is always confirmed. Once accepted, the place
				-- remembers the project and later auto-connects skip this.
				Log.trace("Confirming auto-connect because the server is not known to belong to this place")
				confirmationBehavior = "Always"
			elseif confirmationBehavior == "Initial" then
				Log.trace("Accepting patch without confirmation because auto-connect matched this place")
				return "Accept"
			end
		end

		if confirmationBehavior == "Initial" then
			-- Only confirm if we haven't synced this project yet this session
			if self.knownProjects[serverInfo.projectName] then
				Log.trace(
					"Accepting patch without confirmation because project has already been connected and behavior is set to Initial"
				)
				return "Accept"
			end
		elseif confirmationBehavior == "Large Changes" then
			-- Only confirm if the patch impacts many instances
			if PatchSet.countInstances(patch) < Settings:get("largeChangesConfirmationThreshold") then
				Log.trace(
					"Accepting patch without confirmation because patch is small and behavior is set to Large Changes"
				)
				return "Accept"
			end
		elseif confirmationBehavior == "Unlisted PlaceId" then
			-- Only confirm if the current placeId is not in the servePlaceIds allowlist
			if serverInfo.expectedPlaceIds then
				local isListed = table.find(serverInfo.expectedPlaceIds, game.PlaceId) ~= nil
				if isListed then
					Log.trace(
						"Accepting patch without confirmation because placeId is listed and behavior is set to Unlisted PlaceId"
					)
					return "Accept"
				end
			end
		elseif confirmationBehavior == "Never" then
			Log.trace("Accepting patch without confirmation because behavior is set to Never")
			return "Accept"
		end

		-- The datamodel name gets overwritten by Studio, making confirmation of it intrusive
		-- and unnecessary. This special case allows it to be accepted without confirmation.
		if
			PatchSet.hasAdditions(patch) == false
			and PatchSet.hasRemoves(patch) == false
			and PatchSet.containsOnlyInstance(patch, instanceMap, game)
		then
			local datamodelUpdates = PatchSet.getUpdateForInstance(patch, instanceMap, game)
			if
				datamodelUpdates ~= nil
				and next(datamodelUpdates.changedProperties) == nil
				and datamodelUpdates.changedClassName == nil
			then
				Log.trace("Accepting patch without confirmation because it only contains a datamodel name change")
				return "Accept"
			end
		end

		self:setState({
			connectingText = "Computing diff view...",
		})
		self:setState({
			appStatus = AppStatus.Confirming,
			patchTree = PatchTree.build(patch, instanceMap, { "Property", "Current", "Incoming" }),
			confirmData = {
				serverInfo = serverInfo,
			},
			toolbarIcon = Assets.Images.PluginButton,
		})

		self:addNotification({
			text = string.format(
				"Please accept%sor abort the initializing sync session.",
				Settings:get("twoWaySync") and ", reject, " or " "
			),
			timeout = 7,
		})

		return self.confirmationEvent:Wait()
	end)

	serveSession:start()
end

function App:endSession()
	if self.serveSession == nil then
		return
	end

	Log.trace("Disconnecting session")

	self.serveSession:stop()
	self.serveSession = nil
	self.pendingAutoConnect = nil
	self:setState({
		appStatus = AppStatus.NotConnected,
	})

	if self.cleanupPrecommit ~= nil then
		self.cleanupPrecommit()
	end
	if self.cleanupPostcommit ~= nil then
		self.cleanupPostcommit()
	end

	Log.trace("Session terminated by user")
end

function App:render()
	local pluginName = "Rojo " .. Version.display(Config.version)

	local function createPageElement(appStatus, additionalProps)
		additionalProps = additionalProps or {}

		local props = Dictionary.merge(additionalProps, {
			component = StatusPages[appStatus],
			active = self.state.appStatus == appStatus,
		})

		return e(Page, props)
	end

	return e(StudioPluginContext.Provider, {
		value = self.props.plugin,
	}, {
		e(Theme.StudioProvider, nil, {
			tooltip = e(Tooltip.Provider, nil, {
				gui = e(StudioPluginGui, {
					id = pluginName,
					title = pluginName,
					active = self.state.guiEnabled,
					isEphemeral = false,

					initDockState = Enum.InitialDockState.Right,
					overridePreviousState = false,
					floatingSize = Vector2.new(320, 210),
					minimumSize = Vector2.new(300, 210),

					zIndexBehavior = Enum.ZIndexBehavior.Sibling,

					onInitialState = function(initialState)
						self:setState({
							guiEnabled = initialState,
						})
					end,

					onClose = function()
						self:setState({
							guiEnabled = false,
						})
					end,
				}, {
					Tooltips = e(Tooltip.Container, nil),

					NotConnectedPage = createPageElement(AppStatus.NotConnected, {
						host = self.host,
						onHostChange = self.setHost,
						port = self.port,
						onPortChange = self.setPort,

						onConnect = function()
							self:startSession()
						end,

						autoConnectHere = self.state.autoConnectHere,
						autoConnectStatus = self.state.autoConnectStatus,
						onToggleAutoConnectHere = function()
							self:setAutoConnectHere(not self:isAutoConnectOnHere())
						end,

						onNavigateSettings = function()
							self.backPage = AppStatus.NotConnected
							self:setState({
								appStatus = AppStatus.Settings,
							})
						end,
					}),

					ConfirmingPage = createPageElement(AppStatus.Confirming, {
						confirmData = self.state.confirmData,
						patchTree = self.state.patchTree,
						createPopup = not self.state.guiEnabled,

						onAbort = function()
							self.confirmationBindable:Fire("Abort")
						end,
						onAccept = function()
							self.confirmationBindable:Fire("Accept")
						end,
						onReject = function()
							self.confirmationBindable:Fire("Reject")
						end,
					}),

					Connecting = createPageElement(AppStatus.Connecting, {
						text = self.state.connectingText,
					}),

					Connected = createPageElement(AppStatus.Connected, {
						projectName = self.state.projectName,
						address = self.state.address,
						patchTree = self.state.patchTree,
						patchData = self.state.patchData,
						serveSession = self.serveSession,

						onDisconnect = function()
							self:endSession()
						end,

						onNavigateSettings = function()
							self.backPage = AppStatus.Connected
							self:setState({
								appStatus = AppStatus.Settings,
							})
						end,
					}),

					Settings = createPageElement(AppStatus.Settings, {
						syncActive = self.serveSession ~= nil
							and self.serveSession:getStatus() == ServeSession.Status.Connected,

						onBack = function()
							self:setState({
								appStatus = self.backPage or AppStatus.NotConnected,
							})
						end,
					}),

					Error = createPageElement(AppStatus.Error, {
						errorMessage = self.state.errorMessage,

						onClose = function()
							self:setState({
								appStatus = AppStatus.NotConnected,
								toolbarIcon = Assets.Images.PluginButton,
							})
						end,
					}),
				}),

				RojoNotifications = e("ScreenGui", {
					ZIndexBehavior = Enum.ZIndexBehavior.Sibling,
					ResetOnSpawn = false,
					DisplayOrder = 100,
				}, {
					Notifications = e(Notifications, {
						soundPlayer = self.props.soundPlayer,
						notifications = self.state.notifications,
						onClose = function(id)
							self:closeNotification(id)
						end,
					}),
				}),
			}),

			toggleAction = e(StudioPluginAction, {
				name = "RojoConnection",
				title = "Rojo: Connect/Disconnect",
				description = "Toggles the server for a Rojo sync session",
				icon = Assets.Images.PluginButton,
				bindable = true,
				onTriggered = function()
					if self.serveSession == nil or self.serveSession:getStatus() == ServeSession.Status.NotStarted then
						self:startSession()
					elseif
						self.serveSession ~= nil and self.serveSession:getStatus() == ServeSession.Status.Connected
					then
						self:endSession()
					end
				end,
			}),

			connectAction = e(StudioPluginAction, {
				name = "RojoConnect",
				title = "Rojo: Connect",
				description = "Connects the server for a Rojo sync session",
				icon = Assets.Images.PluginButton,
				bindable = true,
				onTriggered = function()
					if self.serveSession == nil or self.serveSession:getStatus() == ServeSession.Status.NotStarted then
						self:startSession()
					end
				end,
			}),

			disconnectAction = e(StudioPluginAction, {
				name = "RojoDisconnect",
				title = "Rojo: Disconnect",
				description = "Disconnects the server for a Rojo sync session",
				icon = Assets.Images.PluginButton,
				bindable = true,
				onTriggered = function()
					if self.serveSession ~= nil and self.serveSession:getStatus() == ServeSession.Status.Connected then
						self:endSession()
					end
				end,
			}),

			toolbar = e(StudioToolbar, {
				name = pluginName,
			}, {
				button = e(StudioToggleButton, {
					name = "Rojo",
					tooltip = "Show or hide the Rojo panel",
					icon = self.state.toolbarIcon,
					active = self.state.guiEnabled,
					enabled = true,
					onClick = function()
						self:setState(function(state)
							return {
								guiEnabled = not state.guiEnabled,
							}
						end)
					end,
				}),
			}),
		}),
	})
end

return function(props)
	local mergedProps = Dictionary.merge(props, {
		soundPlayer = soundPlayer.new(Settings),
	})

	return e(App, mergedProps)
end
