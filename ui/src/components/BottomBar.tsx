import ChatComposer from "./ChatComposer";
import "./BottomBar.css";

function BottomBar() {
  return (
    <div className="bottom-bar">
      <ChatComposer variant="bottom" />
    </div>
  );
}

export default BottomBar;