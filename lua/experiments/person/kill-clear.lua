-- Person tests: remove `np-person-kill` if anything of it is left.
-- states: server
-- timeout: 10

local g = Group.getByName('np-person-kill')
if g then
  g:destroy()
  return 'removed'
end
return 'nothing left'
