//! Services whose calls later steps build. Every call answers `UNIMPLEMENTED`.

use nanoprobe_proto::v1alpha1::extension_service_server::ExtensionService;
use nanoprobe_proto::v1alpha1::players_service_server::PlayersService;
use nanoprobe_proto::v1alpha1::*;
use tonic::{Request, Response, Status};

use crate::convert::not_built;

/// The Extension service: schemas, author events and data, Lua extensions.
pub struct Extension;

#[tonic::async_trait]
impl ExtensionService for Extension {
    async fn register_schema(
        &self,
        _req: Request<RegisterSchemaRequest>,
    ) -> Result<Response<RegisterSchemaResponse>, Status> {
        Err(not_built("RegisterSchema"))
    }

    async fn publish_event(
        &self,
        _req: Request<PublishEventRequest>,
    ) -> Result<Response<PublishEventResponse>, Status> {
        Err(not_built("PublishEvent"))
    }

    async fn put_data(
        &self,
        _req: Request<PutDataRequest>,
    ) -> Result<Response<PutDataResponse>, Status> {
        Err(not_built("PutData"))
    }

    async fn get_data(
        &self,
        _req: Request<GetDataRequest>,
    ) -> Result<Response<GetDataResponse>, Status> {
        Err(not_built("GetData"))
    }

    async fn register_lua(
        &self,
        _req: Request<RegisterLuaRequest>,
    ) -> Result<Response<RegisterLuaResponse>, Status> {
        Err(not_built("RegisterLua"))
    }

    async fn remove_lua(
        &self,
        _req: Request<RemoveLuaRequest>,
    ) -> Result<Response<RemoveLuaResponse>, Status> {
        Err(not_built("RemoveLua"))
    }

    async fn list_lua(
        &self,
        _req: Request<ListLuaRequest>,
    ) -> Result<Response<ListLuaResponse>, Status> {
        Err(not_built("ListLua"))
    }

    async fn invoke_lua(
        &self,
        _req: Request<InvokeLuaRequest>,
    ) -> Result<Response<InvokeLuaResponse>, Status> {
        Err(not_built("InvokeLua"))
    }

    async fn eval_lua(
        &self,
        _req: Request<EvalLuaRequest>,
    ) -> Result<Response<EvalLuaResponse>, Status> {
        Err(not_built("EvalLua"))
    }
}

/// The Players service: players, occupancy, messages, marks, drawings and menus.
pub struct Players;

#[tonic::async_trait]
impl PlayersService for Players {
    async fn list_players(
        &self,
        _req: Request<ListPlayersRequest>,
    ) -> Result<Response<ListPlayersResponse>, Status> {
        Err(not_built("ListPlayers"))
    }

    async fn list_occupancy(
        &self,
        _req: Request<ListOccupancyRequest>,
    ) -> Result<Response<ListOccupancyResponse>, Status> {
        Err(not_built("ListOccupancy"))
    }

    async fn send_message(
        &self,
        _req: Request<SendMessageRequest>,
    ) -> Result<Response<SendMessageResponse>, Status> {
        Err(not_built("SendMessage"))
    }

    async fn play_sound(
        &self,
        _req: Request<PlaySoundRequest>,
    ) -> Result<Response<PlaySoundResponse>, Status> {
        Err(not_built("PlaySound"))
    }

    async fn send_chat(
        &self,
        _req: Request<SendChatRequest>,
    ) -> Result<Response<SendChatResponse>, Status> {
        Err(not_built("SendChat"))
    }

    async fn put_mark(
        &self,
        _req: Request<PutMarkRequest>,
    ) -> Result<Response<PutMarkResponse>, Status> {
        Err(not_built("PutMark"))
    }

    async fn put_drawing(
        &self,
        _req: Request<PutDrawingRequest>,
    ) -> Result<Response<PutDrawingResponse>, Status> {
        Err(not_built("PutDrawing"))
    }

    async fn put_menu_item(
        &self,
        _req: Request<PutMenuItemRequest>,
    ) -> Result<Response<PutMenuItemResponse>, Status> {
        Err(not_built("PutMenuItem"))
    }

    async fn erase_player(
        &self,
        _req: Request<ErasePlayerRequest>,
    ) -> Result<Response<ErasePlayerResponse>, Status> {
        Err(not_built("ErasePlayer"))
    }
}
