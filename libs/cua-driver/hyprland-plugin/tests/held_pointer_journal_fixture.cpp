// Observer/lifecycle/button methods are extracted verbatim from production.
// This fixture uses REAL libwayland resources, socketpairs and protocol logger.
#include "held_pointer_journal.hpp"
#include "held_phase.hpp"
#include "foreground_route.hpp"
#include <memory>
#include <optional>
#include <vector>
#include <string>
#include <algorithm>
#include <format>
#include <iostream>
#include <cstdlib>
#include <sys/socket.h>
#include <unistd.h>
using namespace cua::hyprland;
constexpr std::size_t kMaxResources=512;
constexpr bool kProduction=true;
struct Surface { bool good() const {return true;} };
struct Seat { struct {std::shared_ptr<Surface> pointerFocus;} m_state; } seat;
Seat* g_pSeatManager=&seat;
namespace PROTO {struct Data {bool active=false;bool dndActive() const {return active;}} data_value;Data* data=&data_value;}
struct Proxy {
    wl_resource* raw;
    bool suppress_press=false, suppress_release=false, suppress_frames=false;
    unsigned release_attempts=0;
    bool good() const { return raw != nullptr; }
    int version() const { return wl_resource_get_version(raw); }
    void sendButton(unsigned time,unsigned code,wl_pointer_button_state state) {
        if(state==WL_POINTER_BUTTON_STATE_RELEASED)++release_attempts;
        if ((state==WL_POINTER_BUTTON_STATE_PRESSED && suppress_press) ||
            (state==WL_POINTER_BUTTON_STATE_RELEASED && suppress_release)) return;
        wl_pointer_send_button(raw,1,time,code,state);
    }
    void sendButton(unsigned,unsigned time,unsigned code,wl_pointer_button_state state) {sendButton(time,code,state);}
    wl_resource* resource() const {return raw;}
    void sendFrame() { if (!suppress_frames && version()>=5) wl_pointer_send_frame(raw); }
};
struct Client { InputRoute route=InputRoute::primary_foreground; };
std::string refusal(const char* code);
struct Held {
    Client* client;
    HeldPhase phase;
    unsigned button=272;
    std::vector<std::weak_ptr<Proxy>> foreground;
    std::weak_ptr<Surface> root={};
    std::vector<std::shared_ptr<Proxy>> independent={};
    struct ObservedPointer {
        std::weak_ptr<Proxy> owner;
        wl_resource* resource=nullptr;
        HeldPointerJournal journal;
        bool destroyed=false;
        bool dnd_withheld=false;
        struct Watch { wl_listener listener{}; ObservedPointer* record=nullptr; } watch;
    };
    std::vector<ObservedPointer> observed={};
    bool press_attempted=false;
};
struct Impl {
    std::optional<Held> held,foreground_debt;
    wl_protocol_logger* held_logger=nullptr;
    std::optional<std::size_t> held_press_slot;
    std::vector<std::weak_ptr<Proxy>> foreground_pointers;
    unsigned held_button=0;
    bool held_cleanup_unconfirmed=false;
    std::string reply;
    void require_foreground(Client&) {}
    unsigned event_ms() const { return 1; }
    unsigned serial() const {return 2;}
    bool held_source(Client&,const std::string&) const {return native_held_phase_live();}
    bool packet_witness(Client&,const std::vector<std::string>&) const {return true;}
    bool available() const {return true;}
    bool layout_qualified() const {return true;}
    bool primary_conflict(Client&) const {return false;}
    bool agent_conflict(Client&) const {return false;}
    void revoke(const char* reason,bool) {held->phase.reason=reason;cleanup_held();}
    void send(Client&,std::string result) {reply=std::move(result);}
    void served_move_source_change(Client& c,const std::vector<std::string>& f,std::uint64_t sequence) {
        // PRODUCTION_MOVE_RETIRE_BRANCH
    }
    // PRODUCTION_OBSERVER
};
std::string refusal(const char* code) {return code;}
void check(bool ok,const char* why) { if (!ok) {std::cerr<<why<<'\n';std::exit(1);} }
struct World {
    wl_display* display=wl_display_create();
    int sockets[2]{};
    wl_client* client=nullptr;
    Client owner;
    Impl lane;
    std::vector<std::shared_ptr<Proxy>> proxies;
    std::shared_ptr<Surface> root=std::make_shared<Surface>();
    World() {
        check(display && socketpair(AF_UNIX,SOCK_STREAM|SOCK_CLOEXEC,0,sockets)==0,"world allocation");
        client=wl_client_create(display,sockets[0]); check(client,"client creation");
        lane.held_logger=wl_display_add_protocol_logger(display,Impl::held_event,&lane);
        check(lane.held_logger,"observer registration");
    }
    std::shared_ptr<Proxy> pointer(unsigned version=5) {
        auto* raw=wl_resource_create(client,&wl_pointer_interface,version,0); check(raw,"pointer creation");
        auto proxy=std::make_shared<Proxy>(raw); proxies.push_back(proxy); return proxy;
    }
    void begin(const std::vector<std::shared_ptr<Proxy>>& captured,InputRoute route=InputRoute::primary_foreground) {
        owner.route=route;
        lane.held.emplace(Held{&owner,HeldPhase{1,"handle",SourceWitness{"root",1,1,{}},HeldPhase::Clock::now()},272,{},{}});
        lane.foreground_pointers.clear();
        lane.held->root=root;seat.m_state.pointerFocus=root;
        for (const auto& p:captured) {
            if(route==InputRoute::primary_foreground) {lane.foreground_pointers.push_back(p);lane.held->foreground.push_back(p);}
            else lane.held->independent.push_back(p);
        }
        check(lane.capture_held_pointers(),"capture originals");
        lane.held->press_attempted=true;
        if(route==InputRoute::primary_foreground)lane.foreground_button(owner,272,true);
        else for(const auto& p:captured) {p->sendButton(1,1,272,WL_POINTER_BUTTON_STATE_PRESSED);p->sendFrame();}
        for (const auto& p:captured) p->sendFrame();
    }
    void release(const std::shared_ptr<Proxy>& p) { p->sendButton(1,272,WL_POINTER_BUTTON_STATE_RELEASED); p->sendFrame(); p->sendFrame(); }
    ~World() {lane.stop_held_logger();wl_client_destroy(client);wl_display_destroy(display);close(sockets[1]);seat.m_state.pointerFocus.reset();}
};
int main() {
    {
        World w;auto p=w.pointer();w.lane.stop_held_logger();
        w.lane.held.emplace(Held{&w.owner,HeldPhase{1,"handle",SourceWitness{"root",1,1,{}},HeldPhase::Clock::now()},272,{p}});
        check(!w.lane.capture_held_pointers() && !w.lane.held_press_slot,"unavailable observer admitted BEGIN");
        check(p->release_attempts==0,"observer failure sent input");
        check(w.lane.held_status_fields().find("\"held_observer\":false")!=std::string::npos &&
            w.lane.held_status_fields().find("\"held_phase_version\":0")!=std::string::npos,"STATUS overclaimed missing observer");
        std::cout<<"STATUS unavailable {"<<w.lane.held_status_fields()<<"}\n";
    }
    {
        World w;auto old=w.pointer(4);
        w.lane.held.emplace(Held{&w.owner,HeldPhase{1,"handle",SourceWitness{"root",1,1,{}},HeldPhase::Clock::now()},272,{old}});
        check(!w.lane.capture_held_pointers(),"pre-frame pointer version overclaimed queue proof");
    }
    {
        World w;auto p=w.pointer();w.begin({p});
        check(w.lane.native_held_phase_live(),"original pressed native phase was not live");
        w.lane.cleanup_held();check(w.lane.held->phase.cleanup_confirmed,"actual cleanup ordinary up did not observe queued release/frame");
        check(w.lane.held_status_fields().find("\"foreground_cleanup_pending\":false")!=std::string::npos &&
            w.lane.held_status_fields().find("\"held_foreground\":true")!=std::string::npos,"STATUS hid clean observer readiness");
        std::cout<<"STATUS ready {"<<w.lane.held_status_fields()<<"}\n";
        check(!w.lane.native_held_phase_live(),"observed button release left MOVE admission live");
        w.lane.held->phase.reason="lease_expired";
        check(w.lane.held_retired_reply(2).find("\"cleanup_confirmed\":true")!=std::string::npos &&
            w.lane.held_retired_reply(2).find("held_lease_expired")!=std::string::npos,"typed retirement lost real event cleanup fact");
        check(p->release_attempts==1,"ordinary up repeated button release");
        w.lane.cleanup_held();check(p->release_attempts==1,"duplicate cleanup replay");
    }
    for (int fault=0;fault<5;++fault) {
        World w; auto a=w.pointer();auto b=w.pointer();auto foreign=w.pointer();
        if (fault==1) a->suppress_press=true;
        if (fault==2) a->suppress_release=true;
        if (fault==3) a->suppress_frames=true;
        if (fault==4) wl_client_post_implementation_error(w.client,"fixture error");
        w.begin({a,b});
        auto& entries=w.lane.held->observed;
        check(entries.size()==2,"bounded original ledger");
        check(entries[0].journal.began()==(fault!=1 && fault!=3 && fault!=4),"silent press/frame/error refusal was promoted");
        w.release(foreign);
        check(!entries[0].journal.release_seen && !entries[1].journal.release_seen,"foreign resource handoff");
        w.release(a);w.release(b);
        check(Impl::observed_cleanup(*w.lane.held)==(fault==0 || fault==1 || fault==4),"release/frame quorum");
        // Missing presses are not an admitted BEGIN, even if cleanup was vacuous.
        if (fault==0) {
            w.lane.held->phase.retire(true);
            Impl::detach_held_watchers(w.lane.held);w.lane.held.reset();
            w.begin({a,b});check(w.lane.held->observed[0].journal.began(),"repeat hold without reload");
        }
    }
    {
        World w;auto p=w.pointer();w.begin({p});
        auto* surface=wl_resource_create(w.client,&wl_surface_interface,1,0);
        p->sendButton(1,272,WL_POINTER_BUTTON_STATE_RELEASED);
        wl_pointer_send_leave(p->raw,1,surface); // native release-before-leave
        check(w.lane.held->observed[0].journal.release_queued,"leave did not prove prior release queueing");
        const auto attempts=p->release_attempts;
        seat.m_state.pointerFocus.reset();w.root.reset();
        std::vector<std::string> fields(14);fields[2]="handle";
        w.lane.served_move_source_change(w.owner,fields,2);
        check(w.lane.held->phase.cleanup_confirmed && p->release_attempts==attempts,"auto root-loss cleanup sent duplicate/successor release");
        check(w.lane.reply.find("\"code\":\"held_retired\"")!=std::string::npos &&
            w.lane.reply.find("\"sequence\":2")!=std::string::npos &&
            w.lane.reply.find("\"cleanup_confirmed\":true")!=std::string::npos,"actual MOVE branch dropped focus-steal queue proof");
        wl_resource_destroy(p->raw);p->raw=nullptr;
        check(w.lane.held->observed[0].destroyed && Impl::observed_cleanup(*w.lane.held),"destruction lost proven completion");
    }
    {
        World w;auto p=w.pointer();w.begin({p});auto* original=p->raw;
        wl_resource_destroy(original);p->raw=nullptr;
        check(w.lane.held->observed[0].destroyed,"destroy listener missed original lifetime");
        check(!w.lane.native_held_phase_live(),"destroyed native resource retained phase admission");
        auto successor=w.pointer();w.release(successor);
        check(!Impl::observed_cleanup(*w.lane.held),"successor resource cleared original release debt");
        w.lane.held->phase.retire(false);w.lane.retain_foreground_debt();
        check(w.lane.held_cleanup_pending(InputRoute::primary_foreground),"original debt disappeared on disconnect");
        check(!w.lane.held_cleanup_pending(InputRoute::independent),"foreground debt blocked independent lane owner");
        check(w.lane.held_status_fields().find("\"foreground_cleanup_pending\":true")!=std::string::npos &&
            w.lane.held_status_fields().find("\"held_cleanup_unconfirmed\":false")!=std::string::npos,"STATUS collapsed foreground and independent debt");
        std::cout<<"STATUS foreground_debt {"<<w.lane.held_status_fields()<<"}\n";
        check(!w.lane.held && w.lane.foreground_debt->client==nullptr,"debt retained dead client/input authority");
    }
    {
        World w;auto p=w.pointer();w.begin({p});auto* surface=wl_resource_create(w.client,&wl_surface_interface,1,0);
        PROTO::data->active=true;
        wl_pointer_send_leave(p->raw,1,surface);p->sendFrame();p->sendFrame(); // DnD: no release
        check(!Impl::observed_cleanup(*w.lane.held),"DnD leave invented release");
        check(w.lane.held->observed[0].dnd_withheld,"observed DnD leave lost its narrow outcome classification");PROTO::data->active=false;
        std::vector<std::string> fields(14);fields[2]="handle";w.lane.served_move_source_change(w.owner,fields,2);
        check(w.lane.reply.find("\"cleanup_confirmed\":false")!=std::string::npos,"MOVE fabricated DnD cleanup");
        w.lane.stop_held_logger();w.lane.held.reset();w.release(p); // no stale callback
        check(!w.lane.held_logger,"unregister lifecycle");
    }
    {
        World w;auto p=w.pointer();w.begin({p});
        w.lane.held->phase.retire(false);w.lane.retain_foreground_debt();
        w.lane.held_cleanup_unconfirmed=true; // independent resource debt is separate
        check(w.lane.held_cleanup_pending(InputRoute::primary_foreground),"missing original release cleared debt");
        w.release(p);
        check(!w.lane.held_cleanup_pending(InputRoute::primary_foreground),"later exact original release/frame proof did not clear foreground debt");
        check(w.lane.held_cleanup_pending(InputRoute::independent),"foreground proof cleared independent debt");
    }
    {
        World w;auto p=w.pointer();w.begin({p});
        w.lane.held->phase.reason="lease_expired";w.lane.held->phase.retire(false);
        check(w.lane.held_retired_reply(2).find("\"cleanup_confirmed\":false")!=std::string::npos,"retirement inferred unobserved cleanup");
        w.release(p);
        check(w.lane.held->phase.cleanup_confirmed && w.lane.held_retired_reply(3).find("\"cleanup_confirmed\":true")!=std::string::npos,"late original event proof did not upgrade typed retired fact");
        check(w.lane.held->phase.retired,"late proof reopened admission");
    }
    {
        for(bool prior_error:{false,true}) {
            World w;auto p=w.pointer();w.begin({p},InputRoute::independent);
            check(w.lane.held->observed[0].journal.began(),"independent original press was not journaled");
            w.lane.held_button=0;seat.m_state.pointerFocus.reset();w.root.reset();
            if(prior_error)wl_client_post_implementation_error(w.client,"independent release fixture error");
            w.lane.cleanup_held();
            check(p->release_attempts==1,"global zero suppressed independent original-button cleanup");
            check(w.lane.held->phase.cleanup_confirmed==!prior_error,"independent alive raw proxy fabricated queue success");
            check(w.lane.held_status_fields().find(prior_error ? "\"held_cleanup_unconfirmed\":true" : "\"held_cleanup_unconfirmed\":false")!=std::string::npos,"STATUS omitted independent cleanup truth");
            if(prior_error)std::cout<<"STATUS independent_debt {"<<w.lane.held_status_fields()<<"}\n";
            w.lane.cleanup_held();check(p->release_attempts==1,"independent retired cleanup repeated release");
        }
    }
    {
        World w;auto p=w.pointer();w.begin({p});w.release(p);
        p->sendButton(1,272,WL_POINTER_BUTTON_STATE_PRESSED);p->sendFrame();p->sendFrame();
        check(!Impl::observed_cleanup(*w.lane.held),"successor press on reused native resource was accepted as old ownership");
    }
    std::cout<<"production observer with real resources, exact lifetimes and queue sentinels passed\n";
}
