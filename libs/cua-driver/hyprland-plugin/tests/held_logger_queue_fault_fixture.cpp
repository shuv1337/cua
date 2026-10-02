// handle_array is extracted verbatim from the qualified libwayland source.
// The production journal parser receives the real ordering, while marshal and
// send functions inject allocation/buffer failure without OOMing the host.
#include "held_pointer_journal.hpp"
#include <iostream>
#include <cstdlib>
using namespace cua::hyprland;
struct wl_connection { bool fail_send=false; unsigned queued=0; };
struct wl_client { bool error=false; wl_connection* connection; };
struct wl_closure { unsigned opcode; const wl_message* message; wl_argument* args; int count; };
bool fail_verify=false,fail_marshal=false;
HeldPointerJournal journal{.button=272};
bool verify_objects(wl_resource*,uint32_t,wl_argument*) {return !fail_verify;}
wl_closure* wl_closure_marshal(wl_object*,unsigned opcode,wl_argument* args,const wl_message* message) {
    static wl_closure closure;
    if (fail_marshal) return nullptr;
    closure={opcode,message,args,opcode==WL_POINTER_BUTTON ? 4 : 0};return &closure;
}
void log_closure(wl_resource* resource,wl_closure* closure,bool) {
    wl_protocol_logger_message event{resource,static_cast<int>(closure->opcode),closure->message,closure->count,closure->args};
    observe_held_pointer_event(journal,WL_PROTOCOL_LOGGER_EVENT,&event);
}
void wl_closure_destroy(wl_closure*) {}
int send_event(wl_closure*,wl_connection* connection) {if(connection->fail_send)return -1;++connection->queued;return 0;}
// PRODUCTION_HANDLE_ARRAY
void check(bool ok,const char* why) {if(!ok){std::cerr<<why<<'\n';std::exit(1);}}
int main() {
    for (int failure=0;failure<7;++failure) {
        wl_connection connection;wl_client client{false,&connection};wl_resource resource{};
        resource.object.interface=&wl_pointer_interface;resource.client=&client;
        journal=HeldPointerJournal{.button=272};fail_verify=false;fail_marshal=false;
        wl_argument args[4]{};args[2].u=272;args[3].u=WL_POINTER_BUTTON_STATE_PRESSED;
        handle_array(&resource,WL_POINTER_BUTTON,args,send_event);
        handle_array(&resource,WL_POINTER_FRAME,nullptr,send_event);
        handle_array(&resource,WL_POINTER_FRAME,nullptr,send_event);
        check(journal.began(),"positive BEGIN sentinels");
        args[3].u=WL_POINTER_BUTTON_STATE_RELEASED;
        if(failure==1)client.error=true;
        if(failure==2)fail_verify=true;
        if(failure==3)fail_marshal=true;
        if(failure==4)connection.fail_send=true;
        handle_array(&resource,WL_POINTER_BUTTON,args,send_event);
        if(failure==5)connection.fail_send=true; // release queued, first frame fails AFTER logger
        handle_array(&resource,WL_POINTER_FRAME,nullptr,send_event);
        if(failure==6)connection.fail_send=true; // queue fact for release/first-frame is already true
        handle_array(&resource,WL_POINTER_FRAME,nullptr,send_event);
        check(journal.cleaned()==(failure==0 || failure==6),"pre-send logger observation fabricated queued release/frame");
        check(journal.release_seen==(failure==0 || failure==4 || failure==5 || failure==6),"marshal boundary classification");
        if(failure==4)check(!journal.release_queued,"failed release queue falsely confirmed");
        if(failure==5)check(journal.release_queued && !journal.release_frame_queued,"failed frame queue falsely confirmed");
    }
    std::cout<<"production handle_array allocation/verify/prior-error/post-logger send failures passed\n";
}
