import { Routes, Route, Navigate } from "react-router-dom";
import Layout from "./components/Layout";
import HomePage from "./pages/HomePage";
import ChatPage from "./pages/ChatPage";
import ProjectPage from "./pages/ProjectPage";
import FileEditorPage from "./pages/FileEditorPage";
import WorkspacePage from "./pages/WorkspacePage";
import ModelsPage from "./pages/ModelsPage";
import ModelHubPage from "./pages/ModelHubPage";
import SettingsPage from "./pages/SettingsPage";
import SecurityPage from "./pages/SecurityPage";
import LogsPage from "./pages/LogsPage";
import HelpPage from "./pages/HelpPage";

function App() {
  return (
    <Layout>
      <Routes>
        <Route path="/" element={<HomePage />} />
        <Route path="/chat" element={<ChatPage />} />
        <Route path="/project" element={<ProjectPage />} />
        <Route path="/editor" element={<FileEditorPage />} />
        <Route path="/workspace" element={<WorkspacePage />} />
        <Route path="/models" element={<ModelsPage />} />
        <Route path="/models/hub" element={<ModelHubPage />} />
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
