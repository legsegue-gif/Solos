#!/usr/bin/env python3
"""Writes the static pages of the Solos site (privacy, terms, support) in
English and Simplified Chinese into <repo>/site. Plain HTML, no scripts.
Run: python3 tools/site/build.py   (the pages in site/ are its output; edit
this file, not them)."""
import os, sys, html

OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "site")
UPDATED = "2026-10-09"
ISSUES = "https://github.com/legsegue-gif/Solos/issues"
SOURCE = "https://github.com/legsegue-gif/Solos"

CSS = """:root{color-scheme:light dark;--bg:#fff;--fg:#1d1d1f;--mute:#6e6e73;--line:#d2d2d7;--accent:#0a66d6}
@media (prefers-color-scheme:dark){:root{--bg:#000;--fg:#f5f5f7;--mute:#a1a1a6;--line:#38383a;--accent:#4da3ff}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:17px/1.6 -apple-system,BlinkMacSystemFont,"SF Pro Text","PingFang SC","Helvetica Neue",Arial,sans-serif}
main{max-width:720px;margin:0 auto;padding:32px 20px 64px}
nav{display:flex;flex-wrap:wrap;gap:6px 18px;font-size:15px;margin-bottom:28px;padding-bottom:14px;border-bottom:1px solid var(--line)}
nav .lang{margin-left:auto}
a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline}
h1{font-size:30px;line-height:1.25;margin:0 0 4px}h2{font-size:21px;margin:34px 0 8px}
p,li{margin:8px 0}ul{padding-left:22px}
.meta{color:var(--mute);font-size:15px;margin:0 0 20px}
.lead{font-size:19px}
table{border-collapse:collapse;width:100%;font-size:15px;margin:12px 0}
th,td{border:1px solid var(--line);padding:8px 10px;text-align:left;vertical-align:top}
th{background:rgba(127,127,127,.12)}
footer{margin-top:48px;padding-top:14px;border-top:1px solid var(--line);color:var(--mute);font-size:14px}
"""

L = {
 "en": dict(
   code="en", home="Solos", privacy="Privacy Policy", terms="Terms of Use", support="Support",
   prefix="", other_prefix="/zh-hans", other_label="简体中文",
   updated="Last updated", source="Source code", fine="Solos is free software under the GNU GPL-3.0 with an additional permission for App Store distribution.",
 ),
 "zh": dict(
   code="zh-Hans", home="嗦螺仕 Solos", privacy="隐私政策", terms="使用条款", support="支持",
   prefix="/zh-hans", other_prefix="", other_label="English",
   updated="最后更新", source="源代码", fine="Solos 是自由软件，采用 GNU GPL-3.0 许可，并附有适用于 App Store 分发的额外许可。",
 ),
}

def page(lang, slug, title, body, desc):
    l = L[lang]
    path = "%s/%s/" % (l["prefix"], slug) if slug else "%s/" % l["prefix"]
    other = L["zh" if lang == "en" else "en"]
    other_path = "%s/%s/" % (other["prefix"], slug) if slug else "%s/" % other["prefix"]
    nav = ('<nav><a href="%s/">%s</a><a href="%s/privacy/">%s</a><a href="%s/terms/">%s</a><a href="%s/support/">%s</a>'
           '<a class="lang" hreflang="%s" href="%s">%s</a></nav>') % (
        l["prefix"], l["home"], l["prefix"], l["privacy"], l["prefix"], l["terms"], l["prefix"], l["support"],
        other["code"], other_path, l["other_label"])
    return """<!doctype html>
<html lang="%s">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>%s</title>
<meta name="description" content="%s">
<link rel="stylesheet" href="/style.css">
</head>
<body>
<main>
%s
%s
<footer>%s <a href="%s">%s</a></footer>
</main>
</body>
</html>
""" % (l["code"], html.escape(title), html.escape(desc), nav, body, l["fine"], SOURCE, l["source"])

def write(rel, text):
    p = os.path.join(OUT, rel)
    os.makedirs(os.path.dirname(p), exist_ok=True)
    open(p, "w", encoding="utf-8").write(text)

# ---------------------------------------------------------------------------
# English
# ---------------------------------------------------------------------------
EN_PRIVACY = """
<h1>Privacy Policy</h1>
<p class="meta">Solos for iPhone and iPad · Last updated %(updated)s</p>

<p class="lead">Solos has no accounts, no servers of ours, no analytics, no ads and no tracking. What you write and what the assistant does stay on your device, except what you choose to send to the AI services <em>you</em> set up.</p>

<h2>1. Who we are</h2>
<p>Solos is made by %(who)s ("we"). Questions about this policy: open an issue at <a href="%(issues)s">%(issues)s</a>%(email)s.</p>

<h2>2. What we collect</h2>
<p><strong>Nothing.</strong> We do not receive your messages, files, contacts, location, health data or any other personal data. Solos contains no analytics, crash-reporting, advertising or tracking code, and does not use the advertising identifier.</p>

<h2>3. What stays on your device</h2>
<ul>
<li><strong>Conversations and settings</strong> are stored in a database inside the app.</li>
<li><strong>API keys</strong> for the services you add are stored in the iOS Keychain. Deleting an endpoint in Settings removes its key.</li>
<li><strong>Workspace files</strong> (what you attach and what the assistant creates) live in the app's Documents folder, which the Files app can show. The Linux environment Solos runs and any program you install in it also live on your device.</li>
<li><strong>Browser cookies and site data</strong> from the built-in browser stay on your device.</li>
<li><strong>Folders you share</strong>: Solos keeps the system's permission to open them again. It does not copy them anywhere.</li>
</ul>
<p>If you back up your device (iCloud or a computer), your device settings decide whether this data is part of the backup.</p>

<h2>4. What leaves your device, and where it goes</h2>
<p>Solos sends data only in the cases below, and only when you use the feature.</p>
<table>
<tr><th>When</th><th>What is sent</th><th>To whom</th></tr>
<tr><td>You send a message</td><td>The conversation: your messages, files and images you attach, and the results of anything the assistant did for you (command output, file contents, and data from the device features you allowed, see section 5).</td><td>The AI service (&ldquo;endpoint&rdquo;) you configured in Settings, with your own API key. It may be a company's service, a relay, or a server on your own network. <strong>Its privacy policy applies to that data</strong>, not ours.</td></tr>
<tr><td>You open the model list</td><td>A request for the list of models, with your API key.</td><td>The same endpoint.</td></tr>
<tr><td>You ask for weather, places or routes</td><td>Coordinates or a place query.</td><td>Apple (WeatherKit, Maps, location lookup), under Apple's privacy policy.</td></tr>
<tr><td>You or the assistant open a web page or download a file</td><td>Ordinary web requests and the cookies the site set.</td><td>The website.</td></tr>
<tr><td>You add a skill from GitHub</td><td>A request for the skill's files.</td><td>GitHub.</td></tr>
<tr><td>You add an MCP server, or the assistant calls one</td><td>The tool call and its arguments.</td><td>The server you added.</td></tr>
<tr><td>You install packages in the Linux environment</td><td>Package requests.</td><td>The package mirror chosen in Settings (for example the official Alpine, PyPI or npm servers, or a mirror you pick).</td></tr>
<tr><td>You run a program that uses the network</td><td>Whatever that program sends.</td><td>Whoever it connects to.</td></tr>
</table>
<p><strong>The assistant can read what you let it read.</strong> If you ask it to look at your calendar, for example, the events it reads become part of the conversation and are sent to your endpoint like any other message. Only share what you are comfortable sending to that service.</p>

<h2>5. Permissions</h2>
<p>Each permission is optional. iOS asks the first time a feature needs it, and you can change your mind in iOS Settings. Solos uses a permission only when you ask the assistant to do something that needs it.</p>
<table>
<tr><th>Permission</th><th>Used for</th></tr>
<tr><td>Calendar, Reminders</td><td>Reading and adding events and reminders you ask for.</td></tr>
<tr><td>Contacts</td><td>Looking up a contact by name when you ask.</td></tr>
<tr><td>Location (while using)</td><td>Weather, places and routes near you when you ask.</td></tr>
<tr><td>Photos</td><td>Listing photos and copying the ones you choose into the workspace. Saving a picture to your photos only when you ask.</td></tr>
<tr><td>Media &amp; Apple Music</td><td>Searching and playing music when you ask.</td></tr>
<tr><td>Health (read and write)</td><td>Reading data such as steps, heart rate, sleep and weight, or recording weight or water, only when you ask. Health data is never used for advertising or sold. If you ask the assistant to read it, the result becomes part of the conversation and is sent to your endpoint (section 4).</td></tr>
<tr><td>Alarms</td><td>Setting alarms and timers when you ask.</td></tr>
<tr><td>Notifications</td><td>Telling you when a long reply has finished while the app is in the background. Notifications are created on your device.</td></tr>
<tr><td>Local network</td><td>Reaching model services on your own network.</td></tr>
</table>

<h2>6. Sharing and selling</h2>
<p>We do not sell, rent or share personal data, because we do not have it. Data sent to an endpoint, Apple or any other service in section 4 is sent by the app at your request and is governed by that service's own terms and privacy policy.</p>

<h2>7. Keeping and deleting data</h2>
<p>You control everything on your device: delete a chat in the app, delete files in the workspace or the Files app, remove an endpoint (this also removes its key), or delete the app to remove its data. Data you sent to an endpoint is kept by that service according to its own policy; ask them to delete it.</p>

<h2>8. Children</h2>
<p>Solos is not directed at children under 13 and we do not knowingly collect any data from anyone.</p>

<h2>9. Changes</h2>
<p>If this policy changes, we will update this page and the date at the top.</p>

<h2>10. Contact</h2>
<p>Open an issue at <a href="%(issues)s">%(issues)s</a>%(email)s.</p>
"""

EN_TERMS = """
<h1>Terms of Use</h1>
<p class="meta">Solos for iPhone and iPad · Last updated %(updated)s</p>

<p>These terms are in addition to Apple's Licensed Application End User License Agreement (the standard EULA) that applies to apps from the App Store. By using Solos you agree to them.</p>

<h2>1. What Solos is</h2>
<p>Solos is an AI assistant app. <strong>It does not include an AI model.</strong> You connect it to AI services of your choice with your own address and API key. Solos then lets the assistant use tools on your device: a Linux environment, files, a browser and, with your permission, device features such as calendar, reminders, contacts, location, photos and health data.</p>

<h2>2. The software licence</h2>
<p>Solos is free software released under the GNU General Public License version 3, with an additional permission that allows distribution through the App Store. The source code is at <a href="%(source)s">%(source)s</a>. These terms do not take away any right the licence gives you.</p>

<h2>3. Your responsibilities</h2>
<ul>
<li><strong>Services and costs.</strong> Using an AI service may cost money and is subject to that service's terms. You are responsible for your API keys, for the charges they cause and for following those terms.</li>
<li><strong>What you send.</strong> What you write, attach or let the assistant read is sent to the service you chose (see the <a href="/privacy/">Privacy Policy</a>). Do not send what you have no right or wish to send.</li>
<li><strong>Lawful use.</strong> Do not use Solos to break the law, to harm others, or to abuse any service.</li>
</ul>

<h2>4. The assistant can make mistakes, and can act</h2>
<ul>
<li>AI answers can be wrong, incomplete or out of date. Check anything that matters before relying on it. Solos is not a substitute for professional medical, legal or financial advice.</li>
<li>The assistant can run commands, create, change and delete files, browse the web and use the features you allowed. It can do this wrongly. Keep copies of what you cannot afford to lose.</li>
<li>Shared folders are read-only until you allow changes for a folder. Only allow changes in folders you are prepared to have changed.</li>
<li>Skills, MCP servers and programs you add or install run with the access you give them. Add only what you trust.</li>
</ul>

<h2>5. Third-party services</h2>
<p>Model services, websites, GitHub, package mirrors, MCP servers and Apple services are not ours. We are not responsible for them, their availability or what they do with data you send them.</p>

<h2>6. No warranty</h2>
<p>Solos is provided &ldquo;as is&rdquo;, without warranty of any kind, to the extent the law allows, as set out in sections 15 and 16 of the GNU General Public License.</p>

<h2>7. Limit of liability</h2>
<p>To the extent the law allows, we are not liable for indirect or consequential loss, lost data, costs charged by third-party services, or anything the assistant does on your device at your request.</p>

<h2>8. Apple</h2>
<p>These terms are between you and us, not Apple. Apple has no obligation to provide maintenance or support for Solos, and is not responsible for claims about it. Apple and its subsidiaries are third-party beneficiaries of the standard EULA and may enforce it against you.</p>

<h2>9. Changes and ending</h2>
<p>We may update these terms; the date above shows the latest version. You may stop using Solos at any time by deleting it.</p>

<h2>10. Where you live</h2>
<p>These terms do not pick one country's law for everyone. Nothing in them limits the mandatory consumer-protection rights you have under the law of the place where you live.</p>

<h2>11. Contact</h2>
<p>Open an issue at <a href="%(issues)s">%(issues)s</a>%(email)s.</p>
"""

EN_SUPPORT = """
<h1>Support</h1>
<p class="meta">Solos for iPhone and iPad</p>

<h2>Get help</h2>
<p>Report a problem or ask a question at <a href="%(issues)s">%(issues)s</a>. Please say what you did, what you expected and what happened, and your iOS version. Do not post API keys or private files.</p>

<h2>Common questions</h2>
<p><strong>Solos says there is no model.</strong> Solos does not include one. Open Settings, tap Add endpoint, enter the address and API key of an AI service, tap Fetch models and choose a model.</p>
<p><strong>Where are my API keys?</strong> In the iOS Keychain, on your device only. Deleting an endpoint in Settings removes its key.</p>
<p><strong>Where is my data?</strong> Chats and settings are in the app. Workspace files are in the Files app under On My iPhone &gt; Solos. See the <a href="/privacy/">Privacy Policy</a>.</p>
<p><strong>Can it see my other files?</strong> Only folders you share in Settings &gt; Files &gt; Shared folders, and they are read-only unless you allow changes.</p>
<p><strong>I do not want the assistant to run commands or touch files.</strong> Turn off AI Agent Mode in the chat's model menu: the chat is then plain text only.</p>
<p><strong>Can I turn off an AI service without deleting it?</strong> Yes: open the endpoint in Settings and turn off Enabled.</p>

<h2>Legal</h2>
<p><a href="/privacy/">Privacy Policy</a> · <a href="/terms/">Terms of Use</a> · <a href="%(source)s">Source code</a></p>
"""

EN_HOME = """
<h1>Solos</h1>
<p class="lead">A personal AI assistant for iPhone and iPad that can use a Linux environment, files, a browser and your device, with the AI service you choose.</p>
<ul>
<li><a href="/privacy/">Privacy Policy</a></li>
<li><a href="/terms/">Terms of Use</a></li>
<li><a href="/support/">Support</a></li>
<li><a href="%(source)s">Source code</a></li>
</ul>
"""

# ---------------------------------------------------------------------------
# Simplified Chinese
# ---------------------------------------------------------------------------
ZH_PRIVACY = """
<h1>隐私政策</h1>
<p class="meta">iPhone 和 iPad 版 Solos · 最后更新 %(updated)s</p>

<p class="lead">Solos 没有账号、没有我们自己的服务器、没有统计分析、没有广告、没有跟踪。你写的内容和助手做的事都留在你的设备上；只有你主动发给<em>你自己配置的</em> AI 服务的那部分才会离开设备。</p>

<h2>1. 我们是谁</h2>
<p>Solos 由 %(who)s（“我们”）开发。对本政策有疑问，请到 <a href="%(issues)s">%(issues)s</a> 提出问题%(email)s。</p>

<h2>2. 我们收集什么</h2>
<p><strong>什么都不收集。</strong>我们收不到你的消息、文件、联系人、位置、健康数据或任何其他个人数据。Solos 不含任何统计分析、崩溃上报、广告或跟踪代码，也不使用广告标识符。</p>

<h2>3. 留在你设备上的内容</h2>
<ul>
<li><strong>对话和设置</strong>保存在应用内的数据库里。</li>
<li>你添加的服务的 <strong>API 密钥</strong>保存在 iOS 钥匙串里。在设置里删除一个端点，它的密钥也会一并删除。</li>
<li><strong>工作区文件</strong>（你附加的文件和助手创建的文件）放在应用的“文档”文件夹里，“文件”App 可以显示它。Solos 运行的 Linux 环境以及你在里面安装的程序也都在你的设备上。</li>
<li>内置浏览器的 <strong>Cookie 和网站数据</strong>留在你的设备上。</li>
<li><strong>你共享的文件夹</strong>：Solos 保存系统给的访问权限，以便再次打开它们，不会把内容复制到别处。</li>
</ul>
<p>如果你备份设备（iCloud 或电脑），这些数据是否进入备份由你的设备设置决定。</p>

<h2>4. 哪些内容会离开设备，发到哪里</h2>
<p>只有下列情况 Solos 才会发送数据，并且只在你使用相应功能时。</p>
<table>
<tr><th>什么时候</th><th>发送什么</th><th>发给谁</th></tr>
<tr><td>你发送一条消息</td><td>对话内容：你的消息、你附加的文件和图片，以及助手替你做的事的结果（命令输出、文件内容，以及你允许的设备功能读到的数据，见第 5 节）。</td><td>你在设置里配置的 AI 服务（“端点”），使用你自己的 API 密钥。它可能是某家公司的服务、中转站，或你自己网络里的服务器。<strong>这些数据适用对方的隐私政策</strong>，而不是我们的。</td></tr>
<tr><td>你打开模型列表</td><td>获取模型列表的请求，带上你的 API 密钥。</td><td>同一个端点。</td></tr>
<tr><td>你查询天气、地点或路线</td><td>坐标或地点查询。</td><td>Apple（WeatherKit、地图、位置查询），适用 Apple 的隐私政策。</td></tr>
<tr><td>你或助手打开网页、下载文件</td><td>普通的网页请求，以及该网站设置的 Cookie。</td><td>对应的网站。</td></tr>
<tr><td>你从 GitHub 添加技能</td><td>获取技能文件的请求。</td><td>GitHub。</td></tr>
<tr><td>你添加 MCP 服务器，或助手调用它</td><td>工具调用及其参数。</td><td>你添加的那个服务器。</td></tr>
<tr><td>你在 Linux 环境里安装软件包</td><td>软件包请求。</td><td>设置里选的软件源（例如官方的 Alpine、PyPI、npm 服务器，或你选的镜像）。</td></tr>
<tr><td>你运行了会联网的程序</td><td>该程序自己发送的内容。</td><td>它连接的对象。</td></tr>
</table>
<p><strong>助手能读到你允许它读的东西。</strong>比如你让它看你的日历，它读到的日程会成为对话的一部分，和其他消息一样发给你的端点。请只分享你放心发给那个服务的内容。</p>

<h2>5. 权限</h2>
<p>每项权限都是可选的。某个功能第一次需要时 iOS 会询问你，你也可以随时在 iOS 设置里改主意。只有当你让助手做需要某项权限的事时，Solos 才会使用它。</p>
<table>
<tr><th>权限</th><th>用途</th></tr>
<tr><td>日历、提醒事项</td><td>读取和添加你要求的日程和提醒。</td></tr>
<tr><td>通讯录</td><td>你要求时按姓名查找联系人。</td></tr>
<tr><td>位置（使用期间）</td><td>你要求时查询你附近的天气、地点和路线。</td></tr>
<tr><td>照片</td><td>列出照片，并把你选中的复制到工作区；只在你要求时把图片存入相册。</td></tr>
<tr><td>媒体与 Apple Music</td><td>你要求时搜索和播放音乐。</td></tr>
<tr><td>健康（读取和写入）</td><td>只在你要求时读取步数、心率、睡眠、体重等数据，或记录体重、饮水。健康数据不会用于广告，也不会出售。如果你让助手读取它，结果会成为对话的一部分，并发给你的端点（第 4 节）。</td></tr>
<tr><td>闹钟</td><td>你要求时设置闹钟和计时器。</td></tr>
<tr><td>通知</td><td>应用在后台时，告诉你一次较长的回复已经完成。通知在你的设备上生成。</td></tr>
<tr><td>本地网络</td><td>连接你自己网络里的模型服务。</td></tr>
</table>

<h2>6. 共享与出售</h2>
<p>我们不出售、不出租、不共享个人数据，因为我们没有这些数据。第 4 节里发给端点、Apple 或其他服务的数据，是应用按你的要求发送的，受对方自己的条款和隐私政策约束。</p>

<h2>7. 数据的保留与删除</h2>
<p>你设备上的一切由你掌控：在应用里删除对话，在工作区或“文件”App 里删除文件，移除一个端点（它的密钥也会删除），或删除应用来清除它的数据。你发给端点的数据由对方按它自己的政策保存，请向对方申请删除。</p>

<h2>8. 儿童</h2>
<p>Solos 不面向 13 岁以下儿童，我们也不会有意收集任何人的数据。</p>

<h2>9. 变更</h2>
<p>如果本政策有变化，我们会更新本页面和顶部的日期。</p>

<h2>10. 联系方式</h2>
<p>请到 <a href="%(issues)s">%(issues)s</a> 提出问题%(email)s。</p>
"""

ZH_TERMS = """
<h1>使用条款</h1>
<p class="meta">iPhone 和 iPad 版 Solos · 最后更新 %(updated)s</p>

<p>本条款是对适用于 App Store 应用的 Apple《许可应用最终用户许可协议》（标准 EULA）的补充。使用 Solos 即表示你同意本条款。</p>

<h2>1. Solos 是什么</h2>
<p>Solos 是一款 AI 助手应用。<strong>它不自带 AI 模型。</strong>你需要用自己的地址和 API 密钥把它连接到你选的 AI 服务。之后 Solos 让助手在你的设备上使用工具：一个 Linux 环境、文件、浏览器，以及在你允许时使用日历、提醒事项、通讯录、位置、照片和健康数据等设备功能。</p>

<h2>2. 软件许可</h2>
<p>Solos 是自由软件，以 GNU 通用公共许可证第 3 版发布，并附有允许通过 App Store 分发的额外许可。源代码在 <a href="%(source)s">%(source)s</a>。本条款不会剥夺该许可证给你的任何权利。</p>

<h2>3. 你的责任</h2>
<ul>
<li><strong>服务与费用。</strong>使用 AI 服务可能产生费用，并受该服务条款约束。你要对自己的 API 密钥、由它产生的费用负责，并遵守这些条款。</li>
<li><strong>你发送的内容。</strong>你写下、附加或让助手读取的内容会发给你选的服务（见<a href="/zh-hans/privacy/">隐私政策</a>）。不要发送你无权发送或不想发送的内容。</li>
<li><strong>合法使用。</strong>不要用 Solos 违法、伤害他人或滥用任何服务。</li>
</ul>

<h2>4. 助手会出错，也会动手</h2>
<ul>
<li>AI 的回答可能有错、不完整或已过时。重要的事情请先核实再依赖。Solos 不能替代专业的医疗、法律或财务建议。</li>
<li>助手可以运行命令，创建、修改和删除文件，浏览网页，并使用你允许的功能。它可能做错。请为不容有失的内容留好副本。</li>
<li>共享文件夹默认只读，只有你为某个文件夹开启“允许修改”后才能改动。请只在你能接受被改动的文件夹里开启。</li>
<li>你添加或安装的技能、MCP 服务器和程序，会以你给它们的权限运行。只添加你信任的。</li>
</ul>

<h2>5. 第三方服务</h2>
<p>模型服务、网站、GitHub、软件源、MCP 服务器和 Apple 的服务都不归我们所有。我们不对它们、它们的可用性，或它们如何处理你发送的数据负责。</p>

<h2>6. 不提供担保</h2>
<p>在法律允许的范围内，Solos 按“现状”提供，不附带任何形式的担保，依照 GNU 通用公共许可证第 15 和 16 条。</p>

<h2>7. 责任限制</h2>
<p>在法律允许的范围内，我们不对间接或后果性损失、数据丢失、第三方服务收取的费用，或助手应你要求在你的设备上做的任何事负责。</p>

<h2>8. 关于 Apple</h2>
<p>本条款是你与我们之间的约定，与 Apple 无关。Apple 没有义务为 Solos 提供维护或支持，也不对与它有关的索赔负责。Apple 及其子公司是标准 EULA 的第三方受益人，可以向你主张该协议。</p>

<h2>9. 变更与终止</h2>
<p>我们可能更新本条款，顶部的日期表示最新版本。你随时可以通过删除应用来停止使用 Solos。</p>

<h2>10. 你所在的地区</h2>
<p>本条款不为所有人指定某一个国家的法律。其中任何内容都不限制你依据所在地法律享有的强制性消费者保护权利。</p>

<h2>11. 联系方式</h2>
<p>请到 <a href="%(issues)s">%(issues)s</a> 提出问题%(email)s。</p>
"""

ZH_SUPPORT = """
<h1>支持</h1>
<p class="meta">iPhone 和 iPad 版 Solos</p>

<h2>获取帮助</h2>
<p>报告问题或提问请到 <a href="%(issues)s">%(issues)s</a>。请说明你做了什么、期望什么、实际发生了什么，以及你的 iOS 版本。不要贴出 API 密钥或私人文件。</p>

<h2>常见问题</h2>
<p><strong>Solos 提示没有模型。</strong>Solos 不自带模型。打开设置，点“添加端点”，填入某个 AI 服务的地址和 API 密钥，点“获取模型”，然后选一个模型。</p>
<p><strong>我的 API 密钥在哪里？</strong>在 iOS 钥匙串里，只在你的设备上。在设置里删除端点，它的密钥也会删除。</p>
<p><strong>我的数据在哪里？</strong>对话和设置在应用里。工作区文件在“文件”App 的“我的 iPhone”&gt; Solos 里。详见<a href="/zh-hans/privacy/">隐私政策</a>。</p>
<p><strong>它能看到我其他的文件吗？</strong>只能看到你在“设置 &gt; 文件 &gt; 共享文件夹”里共享的文件夹，而且除非你允许修改，否则只读。</p>
<p><strong>我不想让助手运行命令或动文件。</strong>在聊天的模型菜单里关闭“AI 智能体模式”，这个聊天就只是纯文字对话。</p>
<p><strong>能不关闭但不删除某个 AI 服务吗？</strong>可以：在设置里打开这个端点，关闭“启用”。</p>

<h2>法律信息</h2>
<p><a href="/zh-hans/privacy/">隐私政策</a> · <a href="/zh-hans/terms/">使用条款</a> · <a href="%(source)s">源代码</a></p>
"""

ZH_HOME = """
<h1>嗦螺仕 Solos</h1>
<p class="lead">一款 iPhone 和 iPad 上的个人 AI 助手，可以使用 Linux 环境、文件、浏览器和你的设备，搭配你自己选的 AI 服务。</p>
<ul>
<li><a href="/zh-hans/privacy/">隐私政策</a></li>
<li><a href="/zh-hans/terms/">使用条款</a></li>
<li><a href="/zh-hans/support/">支持</a></li>
<li><a href="%(source)s">源代码</a></li>
</ul>
"""

VALS = dict(
    updated=UPDATED, issues=ISSUES, source=SOURCE,
    who='the Solos project, an open-source project (<a href="%s">source code</a>)' % SOURCE,
    email="",
)
VALS_ZH = dict(VALS, who='Solos 开源项目（<a href="%s">源代码</a>）' % SOURCE)

def upd(lang, base):
    return base  # the date is substituted by %(updated)s

write("style.css", CSS)
write("index.html", page("en", "", "Solos", EN_HOME % VALS, "Solos, a personal AI assistant for iPhone and iPad."))
write("privacy/index.html", page("en", "privacy", "Privacy Policy · Solos", EN_PRIVACY % VALS, "How Solos handles your data."))
write("terms/index.html", page("en", "terms", "Terms of Use · Solos", EN_TERMS % VALS, "The terms for using Solos."))
write("support/index.html", page("en", "support", "Support · Solos", EN_SUPPORT % VALS, "Get help with Solos."))

write("zh-hans/index.html", page("zh", "", "嗦螺仕 Solos", ZH_HOME % VALS_ZH, "嗦螺仕 Solos，iPhone 和 iPad 上的个人 AI 助手。"))
write("zh-hans/privacy/index.html", page("zh", "privacy", "隐私政策 · 嗦螺仕", ZH_PRIVACY % VALS_ZH, "Solos 如何处理你的数据。"))
write("zh-hans/terms/index.html", page("zh", "terms", "使用条款 · 嗦螺仕", ZH_TERMS % VALS_ZH, "使用 Solos 的条款。"))
write("zh-hans/support/index.html", page("zh", "support", "支持 · 嗦螺仕", ZH_SUPPORT % VALS_ZH, "获取 Solos 的帮助。"))

# So that an unknown path is a 404, not the home page (Pages treats a site
# with no 404.html as a single-page app and answers every path with index.html).
write("404.html", """<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Not found · Solos</title><link rel="stylesheet" href="/style.css"></head>
<body><main><h1>Not found</h1><p>There is nothing at this address. <a href="/">Solos home</a> · <a href="/zh-hans/">简体中文</a></p></main></body></html>
""")
print("written to", OUT)
