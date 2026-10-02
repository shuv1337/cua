// handle_array is extracted verbatim from the qualified libwayland source.
// The production journal parser receives the real ordering, while marshal and
// send functions inject allocation/buffer failure without OOMing the host.
#include "held_pointer_journal.hpp"
#include "held_phase.hpp"
#include "foreground_route.hpp"
#include <iostream>
#include <cstdlib>
#include <memory>
#include <optional>
#include <vector>
using namespace cua::hyprland;
struct wl_connection { bool fail_send=false; unsigned queued=0; };
struct wl_client { bool error=false; wl_connection* connection; };
struct wl_closure { unsigned opcode; const wl_message* message; wl_argument* args; int count; };
bool fail_verify=false,fail_marshal=false;
HeldPointerJournal journal{.button=272};
HeldPointerJournal* journal_ptr=&journal;
bool verify_objects(wl_resource*,uint32_t,wl_argument*) {return !fail_verify;}
wl_closure* wl_closure_marshal(wl_object*,unsigned opcode,wl_argument* args,const wl_message* message) {
    static wl_closure closure;
    if (fail_marshal) return nullptr;
    closure={opcode,message,args,opcode==WL_POINTER_BUTTON ? 4 : 0};return &closure;
}
void log_closure(wl_resource* resource,wl_closure* closure,bool) {
    wl_protocol_logger_message event{resource,static_cast<int>(closure->opcode),closure->message,closure->count,closure->args};
    observe_held_pointer_event(*journal_ptr,WL_PROTOCOL_LOGGER_EVENT,&event);
}
void wl_closure_destroy(wl_closure*) {}
int send_event(wl_closure*,wl_connection* connection) {if(connection->fail_send)return -1;++connection->queued;return 0;}
// PRODUCTION_HANDLE_ARRAY
struct Proxy {
    wl_resource* raw;
    int fault=0;
    unsigned releases=0,release_frames=0;
    wl_resource* resource() const {return fault==7 ? nullptr : raw;}
    bool good() const {return resource()!=nullptr;}
    int version() const {return 5;}
    void sendButton(unsigned,unsigned time,unsigned code,wl_pointer_button_state state) {sendButton(time,code,state);}
    void sendButton(unsigned,unsigned code,wl_pointer_button_state state) {
        if(state==WL_POINTER_BUTTON_STATE_RELEASED) {
            ++releases;
            if(fault==1)raw->client->error=true;
            if(fault==2)fail_verify=true;
            if(fault==3)fail_marshal=true;
            if(fault==4)raw->client->connection->fail_send=true;
        }
        wl_argument args[4]{};args[2].u=code;args[3].u=state;
        handle_array(raw,WL_POINTER_BUTTON,args,send_event);
    }
    void sendFrame() {
        if(releases && (++release_frames==1 ? fault==5 : fault==6))raw->client->connection->fail_send=true;
        handle_array(raw,WL_POINTER_FRAME,nullptr,send_event);
    }
};
struct Surface {bool good() const {return true;}};
struct Seat {struct {std::shared_ptr<Surface> pointerFocus;} m_state;} seat;
Seat* g_pSeatManager=&seat;
struct Actor {InputRoute route=InputRoute::independent;};
struct Held {
    Actor* client;
    HeldPhase phase;
    std::weak_ptr<Surface> root;
    unsigned button=272;
    bool press_attempted=true;
    std::vector<std::shared_ptr<Proxy>> independent;
    struct Observed {std::weak_ptr<Proxy> owner;wl_resource* resource;HeldPointerJournal journal;bool destroyed=false;};
    std::vector<Observed> observed;
};
struct Owner {
    std::optional<Held> held;
    bool held_logger=true,held_cleanup_unconfirmed=false;
    unsigned held_button=0; // deliberately unreliable lane-global counter
    unsigned serial() const {return 1;}
    unsigned event_ms() const {return 1;}
    // PRODUCTION_INDEPENDENT_CLEANUP
};
void check(bool ok,const char* why) {if(!ok){std::cerr<<why<<'\n';std::exit(1);}}
int main() {
    for (int failure=0;failure<7;++failure) {
        wl_connection connection;wl_client client{false,&connection};wl_resource resource{};
        resource.object.interface=&wl_pointer_interface;resource.client=&client;
        journal=HeldPointerJournal{.button=272};journal_ptr=&journal;fail_verify=false;fail_marshal=false;
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
    for(int fault=0;fault<8;++fault) {
        wl_connection connection;wl_client client{false,&connection};wl_resource resource{};
        resource.object.interface=&wl_pointer_interface;resource.client=&client;
        auto proxy=std::make_shared<Proxy>(&resource);
        Actor actor;Owner owner;auto root=std::make_shared<Surface>();
        owner.held.emplace(Held{&actor,HeldPhase{1,"handle",SourceWitness{"root",1,1,{}},HeldPhase::Clock::now()},root,272,true,{proxy},{{{},&resource,HeldPointerJournal{.button=272},false}}});
        journal_ptr=&owner.held->observed.front().journal;fail_verify=false;fail_marshal=false;
        proxy->sendButton(1,1,272,WL_POINTER_BUTTON_STATE_PRESSED);proxy->sendFrame();proxy->sendFrame();
        check(journal_ptr->began(),"independent BEGIN queue proof");
        root.reset();proxy->fault=fault;
        owner.cleanup_held();
        check(owner.held->phase.cleanup_confirmed==(fault==0 || fault==6),"production independent cleanup fabricated queue success from live proxy/global zero");
        check(proxy->releases==(fault==7 ? 0u : 1u),"original independent release was skipped or repeated");
        owner.cleanup_held();check(proxy->releases==(fault==7 ? 0u : 1u),"retired independent release repeated");
    }
    std::cout<<"production handle_array and independent cleanup allocation/verify/prior-error/post-logger failures passed\n";
}
