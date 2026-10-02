// Methods are extracted verbatim from the matching native Hyprland source.
#include "held_pointer_journal.hpp"
#include <algorithm>
#include <cstdint>
#include <memory>
#include <vector>
#include <string>
#include <iostream>
#include <cstdlib>
#include <sys/socket.h>
#include <unistd.h>
using namespace cua::hyprland;
#define LOGM(...) ((void)0)
struct eHIDCapabilityType { static constexpr int HID_INPUT_CAPABILITY_POINTER = 1; };
namespace Time { unsigned steadyNow() { return 1; } unsigned millis(unsigned now) { return now; } }
struct Owner {};
struct SurfaceResource { wl_resource* raw=nullptr; wl_resource* resource() const { return raw; } };
struct Surface { std::shared_ptr<SurfaceResource> value = std::make_shared<SurfaceResource>(); std::shared_ptr<SurfaceResource> getResource() const { return value; } };
template<class T> struct Weak {
    std::weak_ptr<T> value;
    Weak() = default;
    Weak(std::shared_ptr<T> v):value(v) {}
    explicit operator bool() const { return !value.expired(); }
    std::shared_ptr<T> lock() const { return value.lock(); }
    T* operator->() const { return value.lock().get(); }
    void reset() { value.reset(); }
};
struct Resource {
    wl_resource* raw;
    int version() const { return wl_resource_get_version(raw); }
    void sendButton(unsigned serial,unsigned time,unsigned code,wl_pointer_button_state state) { wl_pointer_send_button(raw,serial,time,code,state); }
    void sendLeave(unsigned serial,SurfaceResource* surface) { wl_pointer_send_leave(raw,serial,surface->resource()); }
    void sendFrame() { wl_pointer_send_frame(raw); }
};
struct SeatManager {
    unsigned nextSerial(std::shared_ptr<Owner>,bool = false) { return 1; }
    void clearPointerButtonSerials(std::shared_ptr<Owner>,std::shared_ptr<Surface>,unsigned) {}
    void recordPointerButtonSerial(std::shared_ptr<Owner>,unsigned,std::shared_ptr<Surface>,unsigned) {}
} manager;
SeatManager* g_pSeatManager = &manager;
namespace PROTO {
    struct Seat { int m_currentCaps = 1; } seat_value;
    struct Data { bool active = false; bool dndActive() const { return active; } } data_value;
    Seat* seat=&seat_value; Data* data=&data_value;
}
struct CWLPointerResource {
    Weak<Owner> m_owner;
    Weak<Surface> m_currentSurface;
    std::shared_ptr<Resource> m_resource;
    std::vector<unsigned> m_pressedButtons;
    struct { struct { void reset() {} } destroySurface; } m_listeners;
    void sendLeave();
    void sendButton(uint32_t,uint32_t,wl_pointer_button_state);
    void sendFrame();
};
// NATIVE_RESOURCE_METHODS
int main() {
    for (int fault=0;fault<6;++fault) {
        std::vector<std::string> events;
        struct Observer { wl_resource* original; HeldPointerJournal journal; std::vector<std::string>* events; };
        auto* display=wl_display_create();int sockets[2];if(socketpair(AF_UNIX,SOCK_STREAM|SOCK_CLOEXEC,0,sockets))return 2;
        auto* client=wl_client_create(display,sockets[0]);
        auto* raw=wl_resource_create(client,&wl_pointer_interface,5,0);
        Observer observer{raw,HeldPointerJournal{.button=272},&events};
        auto* logger=wl_display_add_protocol_logger(display,[](void* data,wl_protocol_logger_type direction,const wl_protocol_logger_message* event) {
            auto& o=*static_cast<Observer*>(data);
            if(direction!=WL_PROTOCOL_LOGGER_EVENT || event->resource!=o.original)return;
            observe_held_pointer_event(o.journal,direction,event);
            if(event->message_opcode==WL_POINTER_BUTTON)o.events->push_back(event->arguments[3].u ? "old:press" : "old:release");
            if(event->message_opcode==WL_POINTER_LEAVE)o.events->push_back("old:leave");
        },&observer);
        auto owner=std::make_shared<Owner>(); auto old=std::make_shared<Surface>(); auto successor=std::make_shared<Surface>();
        old->value->raw=wl_resource_create(client,&wl_surface_interface,1,0);
        successor->value->raw=wl_resource_create(client,&wl_surface_interface,1,0);
        CWLPointerResource pointer{owner,old,std::make_shared<Resource>(raw),{}, {}};
        PROTO::seat->m_currentCaps=1;PROTO::data->active=fault==2;
        pointer.sendButton(1,272,WL_POINTER_BUTTON_STATE_PRESSED);
        pointer.sendFrame();pointer.sendFrame();
        if(!observer.journal.began())return 3;
        if(fault==0) pointer.sendButton(1,272,WL_POINTER_BUTTON_STATE_RELEASED); // ordinary up
        if(fault==1 || fault==2) {
            pointer.sendLeave(); // same method used by focus-steal / surface-destroy listener
            pointer.m_currentSurface=Weak<Surface>(successor);
            pointer.sendButton(1,272,WL_POINTER_BUTTON_STATE_RELEASED); // native pressed-set guard must refuse
        }
        if(fault==3) {PROTO::seat->m_currentCaps=0;pointer.sendButton(1,272,WL_POINTER_BUTTON_STATE_RELEASED);}
        if(fault==4) {pointer.m_pressedButtons.clear();pointer.sendButton(1,272,WL_POINTER_BUTTON_STATE_RELEASED);}
        if(fault==5) {wl_client_post_implementation_error(client,"fixture error");pointer.sendButton(1,272,WL_POINTER_BUTTON_STATE_RELEASED);}
        pointer.sendFrame();pointer.sendFrame();
        if(observer.journal.cleaned()!=(fault==0 || fault==1)) {std::cerr<<"native silent refusal / DnD fabricated queue fact\n";return 4;}
        const std::vector<std::string> expected=fault==0 ? std::vector<std::string>{"old:press","old:release"} :
            fault==1 ? std::vector<std::string>{"old:press","old:release","old:leave"} :
            fault==2 ? std::vector<std::string>{"old:press","old:leave"} : std::vector<std::string>{"old:press"};
        if (events!=expected) { std::cerr << "native original-resource release ordering failed\n"; return 5; }
        wl_protocol_logger_destroy(logger);wl_client_destroy(client);wl_display_destroy(display);close(sockets[1]);
    }
    std::cout << "extracted native methods through real logger: ordinary up / auto leave / DnD / silent refusal passed\n";
}
