// Methods are extracted verbatim from the matching native Hyprland source.
#include <algorithm>
#include <cstdint>
#include <memory>
#include <vector>
#include <string>
#include <iostream>
#include <cstdlib>
#define LOGM(...) ((void)0)
enum wl_pointer_button_state { WL_POINTER_BUTTON_STATE_RELEASED, WL_POINTER_BUTTON_STATE_PRESSED };
struct eHIDCapabilityType { static constexpr int HID_INPUT_CAPABILITY_POINTER = 1; };
namespace Time { unsigned steadyNow() { return 1; } unsigned millis(unsigned now) { return now; } }
struct Owner {};
struct SurfaceResource { bool resource() const { return true; } };
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
    std::vector<std::string>* events;
    void sendButton(unsigned,unsigned,unsigned,wl_pointer_button_state state) { events->push_back(state == WL_POINTER_BUTTON_STATE_RELEASED ? "old:release" : "old:press"); }
    void sendLeave(unsigned,SurfaceResource*) { events->push_back("old:leave"); }
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
};
// NATIVE_RESOURCE_METHODS
int main() {
    for (bool dnd : {false,true}) {
        std::vector<std::string> events;
        auto owner=std::make_shared<Owner>(); auto old=std::make_shared<Surface>(); auto successor=std::make_shared<Surface>();
        CWLPointerResource pointer{owner,old,std::make_shared<Resource>(&events),{}, {}};
        PROTO::data->active=dnd;
        pointer.sendButton(1,272,WL_POINTER_BUTTON_STATE_PRESSED);
        pointer.sendLeave(); // focus-steal and original surface-destroy callback
        pointer.m_currentSurface=Weak<Surface>(successor);
        pointer.sendButton(1,272,WL_POINTER_BUTTON_STATE_RELEASED); // must not release successor
        pointer.sendLeave();
        const std::vector<std::string> expected=dnd ? std::vector<std::string>{"old:press","old:leave","old:leave"} : std::vector<std::string>{"old:press","old:release","old:leave","old:leave"};
        if (events!=expected) { std::cerr << "native original-resource release ordering failed\n"; return 1; }
    }
    std::cout << "native foreground resource ordering and explicit DnD loss limit passed\n";
}
