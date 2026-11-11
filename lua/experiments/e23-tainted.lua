-- E23: whether loading the agent DLL changes DCS.getTaintedFiles() or the
-- tainted categories, with the DLL open in the hook and server states. With
-- the hooks and no DLL it returned nil (LIVE, S3). What a client sees needs
-- a client. The rule: recorded; a change is raised with the owner.
-- states: hook

local E = ...
E.lib('agent').open()
local ok, value = E.cross('server', "E.lib('agent').open() return 'opened'")
E.observe('server_opened', ok and value, { info = true })
E.observe('tainted_files', E.pack(DCS.getTaintedFiles()), { info = true })
local has, categories = pcall(DCS.getTaintedCategories)
E.observe('tainted_categories', has and E.pack(categories) or tostring(categories), { info = true })
E.observe('tainted_files_nil', DCS.getTaintedFiles() == nil, { wine = true })
return 'done'
