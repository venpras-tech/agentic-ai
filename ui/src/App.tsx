import { Routes, Route, Navigate } from "react-router-dom";
import Layout from "./components/Layout";
import ChatPage from "./pages/ChatPage";
import FileEditorPage from "./pages/FileEditorPage";
import WorkspacePage from "./pages/WorkspacePage";
import AgentsPage from "./pages/AgentsPage";
import ModelsPage from "./pages/ModelsPage";
import ModelHubPage from "./pages/ModelHubPage";
import LlamaCppPage from "./pages/LlamaCppPage";
import ImagesPage from "./pages/ImagesPage";
import VideoPage from "./pages/VideoPage";
import SettingsPage from "./pages/SettingsPage";
import SecurityPage from "./pages/SecurityPage";
import LogsPage from "./pages/LogsPage";
import HelpPage from "./pages/HelpPage";

function App() {
  return (
    <Layout>
      <Routes>
        <Route path="/" element={<ChatPage />} />
        <Route path="/chat" element={<Navigate to="/" replace />} />
        <Route path="/project" element={<Navigate to="/workspace" replace />} />
        <Route path="/editor" element={<FileEditorPage />} />
        <Route path="/workspace" element={<WorkspacePage />} />
        <Route path="/agents" element={<AgentsPage />} />
        <Route path="/models" element={<ModelsPage />} />
        <Route path="/models/hub" element={<ModelHubPage />} />
        <Route path="/llama-cpp" element={<LlamaCppPage />} />
        <Route path="/images" element={<ImagesPage />} />
        <Route path="/video" element={<VideoPage />} />
        <Route path="/settings" element={<SettingsPage />} />
        <Route path="/security" element={<SecurityPage />} />
        <Route path="/logs" element={<LogsPage />} />
        <Route path="/help" element={<HelpPage />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Routes>
    </Layout>
  );
}

export default App;
