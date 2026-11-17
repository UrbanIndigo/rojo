local Rojo = script:FindFirstAncestor("Rojo")
local Plugin = Rojo.Plugin
local Packages = Rojo.Packages

local Roact = require(Packages.Roact)

local Config = require(Plugin.Config)

local Theme = require(Plugin.App.Theme)
local BorderedContainer = require(Plugin.App.Components.BorderedContainer)
local TextButton = require(Plugin.App.Components.TextButton)
local Header = require(Plugin.App.Components.Header)
local Checkbox = require(Plugin.App.Components.Checkbox)
local Tooltip = require(Plugin.App.Components.Tooltip)

local PORT_WIDTH = 74
local DIVIDER_WIDTH = 1
local HOST_OFFSET = 12

local e = Roact.createElement

local function AddressEntry(props)
	return Theme.with(function(theme)
		return e(BorderedContainer, {
			transparency = props.transparency,
			size = UDim2.new(1, 0, 0, 36),
			layoutOrder = props.layoutOrder,
		}, {
			Host = e("TextBox", {
				Text = props.host or "",
				FontFace = theme.Font.Code,
				TextSize = theme.TextSize.Large,
				TextColor3 = theme.AddressEntry.TextColor,
				TextXAlignment = Enum.TextXAlignment.Left,
				TextTransparency = props.transparency,
				PlaceholderText = Config.defaultHost,
				PlaceholderColor3 = theme.AddressEntry.PlaceholderColor,
				ClearTextOnFocus = false,

				Size = UDim2.new(1, -(HOST_OFFSET + DIVIDER_WIDTH + PORT_WIDTH), 1, 0),
				Position = UDim2.new(0, HOST_OFFSET, 0, 0),

				ClipsDescendants = true,
				BackgroundTransparency = 1,

				[Roact.Change.Text] = function(object)
					if props.onHostChange ~= nil then
						props.onHostChange(object.Text)
					end
				end,
			}),

			Port = e("TextBox", {
				Text = props.port or "",
				FontFace = theme.Font.Code,
				TextSize = theme.TextSize.Large,
				TextColor3 = theme.AddressEntry.TextColor,
				TextTransparency = props.transparency,
				PlaceholderText = Config.defaultPort,
				PlaceholderColor3 = theme.AddressEntry.PlaceholderColor,
				ClearTextOnFocus = false,

				Size = UDim2.new(0, PORT_WIDTH, 1, 0),
				Position = UDim2.new(1, 0, 0, 0),
				AnchorPoint = Vector2.new(1, 0),

				ClipsDescendants = true,
				BackgroundTransparency = 1,

				[Roact.Change.Text] = function(object)
					local text = object.Text
					text = text:gsub("%D", "")
					object.Text = text

					if props.onPortChange ~= nil then
						props.onPortChange(text)
					end
				end,
			}, {
				Divider = e("Frame", {
					BackgroundColor3 = theme.BorderedContainer.BorderColor,
					BackgroundTransparency = props.transparency,
					Size = UDim2.new(0, DIVIDER_WIDTH, 1, 0),
					AnchorPoint = Vector2.new(1, 0),
					BorderSizePixel = 0,
				}),
			}),
		})
	end)
end

-- A checkbox for auto-connect in this place, and what the last look for a
-- server found.
local function AutoConnectRow(props)
	return Theme.with(function(theme)
		local text = if not props.enabled
			then "Auto-connect off for this place"
			else props.status or "Auto-connect: waiting for rojo serve"

		return e("Frame", {
			Size = UDim2.new(1, 0, 0, 28),
			LayoutOrder = props.layoutOrder,
			BackgroundTransparency = 1,
		}, {
			Checkbox = e(Checkbox, {
				active = props.enabled,
				transparency = props.transparency,
				onClick = props.onToggle,
			}),

			Label = e("TextLabel", {
				Text = text,
				FontFace = theme.Font.Main,
				TextSize = theme.TextSize.Small,
				TextColor3 = theme.SubTextColor,
				TextTransparency = props.transparency,
				TextXAlignment = Enum.TextXAlignment.Left,
				TextTruncate = Enum.TextTruncate.AtEnd,

				Size = UDim2.new(1, -36, 1, 0),
				Position = UDim2.new(0, 36, 0, 0),
				BackgroundTransparency = 1,
			}, {
				Tip = e(Tooltip.Trigger, {
					text = "Connect without pressing Connect when 'rojo serve --autoconnect' serves this place. "
						.. "The port is found for you.",
				}),
			}),
		})
	end)
end

local NotConnectedPage = Roact.Component:extend("NotConnectedPage")

function NotConnectedPage:render()
	return Roact.createFragment({
		Header = e(Header, {
			transparency = self.props.transparency,
			layoutOrder = 1,
		}),

		AddressEntry = e(AddressEntry, {
			host = self.props.host,
			port = self.props.port,
			onHostChange = self.props.onHostChange,
			onPortChange = self.props.onPortChange,
			transparency = self.props.transparency,
			layoutOrder = 2,
		}),

		AutoConnect = e(AutoConnectRow, {
			enabled = self.props.autoConnectHere,
			status = self.props.autoConnectStatus,
			onToggle = self.props.onToggleAutoConnectHere,
			transparency = self.props.transparency,
			layoutOrder = 3,
		}),

		Buttons = e("Frame", {
			Size = UDim2.new(1, 0, 0, 34),
			LayoutOrder = 4,
			BackgroundTransparency = 1,
			ZIndex = 2,
		}, {
			Settings = e(TextButton, {
				text = "Settings",
				style = "Bordered",
				transparency = self.props.transparency,
				layoutOrder = 1,
				onClick = self.props.onNavigateSettings,
			}, {
				Tip = e(Tooltip.Trigger, {
					text = "View and modify plugin settings",
				}),
			}),

			Connect = e(TextButton, {
				text = "Connect",
				style = "Solid",
				transparency = self.props.transparency,
				layoutOrder = 2,
				onClick = self.props.onConnect,
			}, {
				Tip = e(Tooltip.Trigger, {
					text = "Connect to a Rojo sync server",
				}),
			}),

			Layout = e("UIListLayout", {
				HorizontalAlignment = Enum.HorizontalAlignment.Right,
				FillDirection = Enum.FillDirection.Horizontal,
				SortOrder = Enum.SortOrder.LayoutOrder,
				Padding = UDim.new(0, 10),
			}),
		}),

		Layout = e("UIListLayout", {
			HorizontalAlignment = Enum.HorizontalAlignment.Center,
			VerticalAlignment = Enum.VerticalAlignment.Center,
			FillDirection = Enum.FillDirection.Vertical,
			SortOrder = Enum.SortOrder.LayoutOrder,
			Padding = UDim.new(0, 10),
		}),

		Padding = e("UIPadding", {
			PaddingLeft = UDim.new(0, 20),
			PaddingRight = UDim.new(0, 20),
		}),
	})
end

return NotConnectedPage
