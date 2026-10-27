from datetime import datetime
from typing import TYPE_CHECKING, Optional

from sqlalchemy import DateTime, ForeignKey, String, Text, func
from sqlalchemy.dialects.postgresql import JSON, UUID
from sqlalchemy.orm import Mapped, mapped_column, relationship

from app.models.base import Base, UUIDMixin, TimestampMixin

if TYPE_CHECKING:
    from app.models.notification import NotificationChannel


class WebhookInbox(Base, UUIDMixin, TimestampMixin):
    __tablename__ = "webhook_inboxes"

    slug: Mapped[str] = mapped_column(String(100), nullable=False, unique=True, index=True)
    description: Mapped[Optional[str]] = mapped_column(String(500), nullable=True)
    forward_url: Mapped[Optional[str]] = mapped_column(Text, nullable=True)
    notify_email: Mapped[Optional[str]] = mapped_column(String(500), nullable=True)
    email_subject_prefix: Mapped[Optional[str]] = mapped_column(String(200), nullable=True)
    sender_name: Mapped[Optional[str]] = mapped_column(String(200), nullable=True)
    turnstile_secret: Mapped[Optional[str]] = mapped_column(String(200), nullable=True)
    is_active: Mapped[bool] = mapped_column(default=True)

    events: Mapped[list["WebhookEvent"]] = relationship(
        back_populates="inbox", cascade="all, delete-orphan"
    )
    channels: Mapped[list["NotificationChannel"]] = relationship(
        back_populates="inbox", cascade="all, delete-orphan"
    )


class WebhookEvent(Base, UUIDMixin):
    __tablename__ = "webhook_events"

    inbox_id: Mapped["UUID"] = mapped_column(
        UUID(as_uuid=True), ForeignKey("webhook_inboxes.id", ondelete="CASCADE"), nullable=False
    )
    method: Mapped[str] = mapped_column(String(10), nullable=False)
    headers: Mapped[dict] = mapped_column(JSON, default=dict)
    body: Mapped[Optional[dict]] = mapped_column(JSON, nullable=True)
    query_params: Mapped[dict] = mapped_column(JSON, default=dict)
    source_ip: Mapped[Optional[str]] = mapped_column(String(45), nullable=True)
    received_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), server_default=func.now(), nullable=False
    )

    inbox: Mapped["WebhookInbox"] = relationship(back_populates="events")
